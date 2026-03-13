/// Hash prefixes used in XRPL for domain separation.
/// These are prepended to data before hashing to ensure different types
/// of objects produce different hashes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HashPrefix(pub u32);

impl HashPrefix {
    /// Transaction ID calculation (TXN\0).
    pub const TRANSACTION_ID: Self = Self(0x54584E00);
    /// Transaction + metadata node (SND\0).
    pub const TX_NODE: Self = Self(0x534E4400);
    /// Account state leaf node (MLN\0).
    pub const LEAF_NODE: Self = Self(0x4D4C4E00);
    /// Inner node in V1 tree (MIN\0).
    pub const INNER_NODE: Self = Self(0x4D494E00);
    /// Ledger master data (LWR\0).
    pub const LEDGER_MASTER: Self = Self(0x4C575200);
    /// Transaction signing prefix (STX\0).
    pub const TX_SIGN: Self = Self(0x53545800);
    /// Multi-sign prefix (SMT\0).
    pub const TX_MULTI_SIGN: Self = Self(0x534D5400);
    /// Validation signing (VAL\0).
    pub const VALIDATION: Self = Self(0x56414C00);
    /// Proposal signing (PRP\0).
    pub const PROPOSAL: Self = Self(0x50525000);
    /// Manifest (MAN\0).
    pub const MANIFEST: Self = Self(0x4D414E00);
    /// Payment channel claim (CLM\0).
    pub const PAYMENT_CHANNEL_CLAIM: Self = Self(0x434C4D00);
    /// Credential (CRD\0).
    pub const CREDENTIAL: Self = Self(0x43524400);
    /// Batch (BCH\0).
    pub const BATCH: Self = Self(0x42434800);

    /// Return the prefix as 4 big-endian bytes.
    pub fn to_bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}
