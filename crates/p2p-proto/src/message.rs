/// Peer protocol message types (rippled-compatible).
///
/// Values match rippled's `MessageType` enum in `xrpl.proto`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum MessageType {
    Hello = 1,
    Manifests = 2,
    Ping = 3,
    Cluster = 5,
    Endpoints = 15,
    Transaction = 30,
    GetLedger = 31,
    LedgerData = 32,
    ProposeSet = 33,
    StatusChange = 34,
    HaveSet = 35,
    Validation = 41,
    GetObjects = 42,
    ValidatorList = 54,
    Squelch = 55,
    ValidatorListCollection = 56,
    HaveTransactions = 63,
    Transactions = 64,
}

impl MessageType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Hello),
            2 => Some(Self::Manifests),
            3 => Some(Self::Ping),
            5 => Some(Self::Cluster),
            15 => Some(Self::Endpoints),
            30 => Some(Self::Transaction),
            31 => Some(Self::GetLedger),
            32 => Some(Self::LedgerData),
            33 => Some(Self::ProposeSet),
            34 => Some(Self::StatusChange),
            35 => Some(Self::HaveSet),
            41 => Some(Self::Validation),
            42 => Some(Self::GetObjects),
            54 => Some(Self::ValidatorList),
            55 => Some(Self::Squelch),
            56 => Some(Self::ValidatorListCollection),
            63 => Some(Self::HaveTransactions),
            64 => Some(Self::Transactions),
            _ => None,
        }
    }
}
