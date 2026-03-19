/// Peer protocol message types.
///
/// Wire format: `[4-byte type][4-byte length][protobuf payload]`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum MessageType {
    Hello = 1,
    Ping = 3,
    GetPeers = 5,
    Peers = 6,
    Transaction = 15,
    GetLedger = 16,
    LedgerData = 17,
    ProposeSet = 33,
    StatusChange = 34,
    Validation = 41,
    Manifest = 42,
}

impl MessageType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Hello),
            3 => Some(Self::Ping),
            5 => Some(Self::GetPeers),
            6 => Some(Self::Peers),
            15 => Some(Self::Transaction),
            16 => Some(Self::GetLedger),
            17 => Some(Self::LedgerData),
            33 => Some(Self::ProposeSet),
            34 => Some(Self::StatusChange),
            41 => Some(Self::Validation),
            42 => Some(Self::Manifest),
            _ => None,
        }
    }
}
