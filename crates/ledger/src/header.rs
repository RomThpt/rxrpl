use rxrpl_crypto::sha512_half::sha512_half;
use rxrpl_primitives::Hash256;

/// Hash prefix for ledger master hash computation.
/// "LWR\0" (0x4C575200)
const HASH_PREFIX_LEDGER_MASTER: [u8; 4] = [0x4C, 0x57, 0x52, 0x00];

/// Ripple epoch: seconds between Unix epoch and 2000-01-01T00:00:00Z.
pub const RIPPLE_EPOCH_OFFSET: u64 = 946_684_800;

/// Initial total XRP supply in drops (100 billion XRP).
pub const INITIAL_XRP_DROPS: u64 = 100_000_000_000_000_000;

/// Close flag: no consensus agreement on close time.
pub const CLOSE_FLAG_NO_CONSENSUS_TIME: u8 = 0x01;

/// The header of an XRPL ledger.
///
/// Contains metadata about the ledger including sequence number,
/// tree hashes, timestamps, and total XRP supply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerHeader {
    /// Ledger sequence number (starting from 1 for genesis).
    pub sequence: u32,
    /// Total XRP in drops remaining in the network.
    pub drops: u64,
    /// Hash of the parent ledger (all zeros for genesis).
    pub parent_hash: Hash256,
    /// Root hash of the transaction SHAMap.
    pub tx_hash: Hash256,
    /// Root hash of the account state SHAMap.
    pub account_hash: Hash256,
    /// Parent ledger's close time (seconds since ripple epoch).
    pub parent_close_time: u32,
    /// This ledger's close time (seconds since ripple epoch).
    pub close_time: u32,
    /// Resolution of close time in seconds.
    pub close_time_resolution: u8,
    /// Flags about how the ledger was closed.
    pub close_flags: u8,
    /// The computed hash of this ledger header (populated on close).
    pub hash: Hash256,
}

/// Size of the raw binary ledger header (without hash prefix).
pub const RAW_HEADER_SIZE: usize = 4 + 8 + 32 + 32 + 32 + 4 + 4 + 1 + 1; // 118 bytes

impl LedgerHeader {
    /// Deserialize a ledger header from the raw binary format used by rippled.
    ///
    /// Format (big-endian): sequence(4) || drops(8) || parent_hash(32) ||
    /// tx_hash(32) || account_hash(32) || parent_close_time(4) ||
    /// close_time(4) || close_time_resolution(1) || close_flags(1)
    ///
    /// Total: 118 bytes. The ledger hash is computed from these fields.
    pub fn from_raw_bytes(data: &[u8]) -> Option<Self> {
        if data.len() != RAW_HEADER_SIZE {
            return None;
        }
        let mut pos = 0;

        let sequence = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?);
        pos += 4;

        if sequence == 0 {
            return None;
        }

        let drops = u64::from_be_bytes(data[pos..pos + 8].try_into().ok()?);
        pos += 8;

        // Total XRP supply is 100 billion drops max.
        if drops > 100_000_000_000_000_000 {
            return None;
        }
        let parent_hash = Hash256::new(data[pos..pos + 32].try_into().ok()?);
        pos += 32;
        let tx_hash = Hash256::new(data[pos..pos + 32].try_into().ok()?);
        pos += 32;
        let account_hash = Hash256::new(data[pos..pos + 32].try_into().ok()?);
        pos += 32;
        let parent_close_time = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?);
        pos += 4;
        let close_time = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?);
        pos += 4;
        let close_time_resolution = data[pos];
        pos += 1;
        let close_flags = data[pos];

        let mut header = Self {
            sequence,
            drops,
            parent_hash,
            tx_hash,
            account_hash,
            parent_close_time,
            close_time,
            close_time_resolution,
            close_flags,
            hash: Hash256::ZERO,
        };
        header.hash = header.compute_hash();
        Some(header)
    }

    /// Create a new header with default values.
    pub fn new() -> Self {
        Self {
            sequence: 0,
            drops: INITIAL_XRP_DROPS,
            parent_hash: Hash256::ZERO,
            tx_hash: Hash256::ZERO,
            account_hash: Hash256::ZERO,
            parent_close_time: 0,
            close_time: 0,
            close_time_resolution: 30,
            close_flags: 0,
            hash: Hash256::ZERO,
        }
    }

    /// Compute the ledger hash from the header fields.
    ///
    /// Hash = SHA-512-Half(
    ///     prefix || sequence || drops || parent_hash ||
    ///     tx_hash || account_hash || parent_close_time ||
    ///     close_time || close_time_resolution || close_flags
    /// )
    pub fn compute_hash(&self) -> Hash256 {
        let seq_bytes = self.sequence.to_be_bytes();
        let drops_bytes = self.drops.to_be_bytes();
        let parent_close_bytes = self.parent_close_time.to_be_bytes();
        let close_bytes = self.close_time.to_be_bytes();
        let resolution = [self.close_time_resolution];
        let flags = [self.close_flags];

        sha512_half(&[
            &HASH_PREFIX_LEDGER_MASTER,
            &seq_bytes,
            &drops_bytes,
            self.parent_hash.as_bytes(),
            self.tx_hash.as_bytes(),
            self.account_hash.as_bytes(),
            &parent_close_bytes,
            &close_bytes,
            &resolution,
            &flags,
        ])
    }

    /// Returns true if close time was agreed upon by consensus.
    pub fn close_time_agreed(&self) -> bool {
        self.close_flags & CLOSE_FLAG_NO_CONSENSUS_TIME == 0
    }

    /// Serialize the header to the raw 118-byte format used on the wire
    /// (matches `from_raw_bytes`).
    pub fn to_raw_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(RAW_HEADER_SIZE);
        buf.extend_from_slice(&self.sequence.to_be_bytes());
        buf.extend_from_slice(&self.drops.to_be_bytes());
        buf.extend_from_slice(self.parent_hash.as_bytes());
        buf.extend_from_slice(self.tx_hash.as_bytes());
        buf.extend_from_slice(self.account_hash.as_bytes());
        buf.extend_from_slice(&self.parent_close_time.to_be_bytes());
        buf.extend_from_slice(&self.close_time.to_be_bytes());
        buf.push(self.close_time_resolution);
        buf.push(self.close_flags);
        debug_assert_eq!(buf.len(), RAW_HEADER_SIZE);
        buf
    }
}

impl Default for LedgerHeader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_header() {
        let h = LedgerHeader::new();
        assert_eq!(h.sequence, 0);
        assert_eq!(h.drops, INITIAL_XRP_DROPS);
        assert_eq!(h.close_time_resolution, 30);
        assert!(h.close_time_agreed());
    }

    #[test]
    fn hash_deterministic() {
        let mut h = LedgerHeader::new();
        h.sequence = 1;
        let hash1 = h.compute_hash();
        let hash2 = h.compute_hash();
        assert_eq!(hash1, hash2);
        assert!(!hash1.is_zero());
    }

    #[test]
    fn different_sequences_different_hashes() {
        let mut h1 = LedgerHeader::new();
        h1.sequence = 1;
        let mut h2 = LedgerHeader::new();
        h2.sequence = 2;
        assert_ne!(h1.compute_hash(), h2.compute_hash());
    }

    #[test]
    fn close_flag_no_consensus() {
        let mut h = LedgerHeader::new();
        assert!(h.close_time_agreed());
        h.close_flags = CLOSE_FLAG_NO_CONSENSUS_TIME;
        assert!(!h.close_time_agreed());
    }

    #[test]
    fn from_raw_bytes_roundtrip() {
        let mut h = LedgerHeader::new();
        h.sequence = 42;
        h.drops = 99_999_000_000_000_000;
        h.parent_hash = Hash256::new([0x11; 32]);
        h.tx_hash = Hash256::new([0x22; 32]);
        h.account_hash = Hash256::new([0x33; 32]);
        h.parent_close_time = 1000;
        h.close_time = 1010;
        h.close_time_resolution = 10;
        h.close_flags = 0;

        // Serialize in the same order as compute_hash (without prefix).
        let mut buf = Vec::with_capacity(RAW_HEADER_SIZE);
        buf.extend_from_slice(&h.sequence.to_be_bytes());
        buf.extend_from_slice(&h.drops.to_be_bytes());
        buf.extend_from_slice(h.parent_hash.as_bytes());
        buf.extend_from_slice(h.tx_hash.as_bytes());
        buf.extend_from_slice(h.account_hash.as_bytes());
        buf.extend_from_slice(&h.parent_close_time.to_be_bytes());
        buf.extend_from_slice(&h.close_time.to_be_bytes());
        buf.push(h.close_time_resolution);
        buf.push(h.close_flags);
        assert_eq!(buf.len(), RAW_HEADER_SIZE);

        let parsed = LedgerHeader::from_raw_bytes(&buf).unwrap();
        assert_eq!(parsed.sequence, 42);
        assert_eq!(parsed.drops, 99_999_000_000_000_000);
        assert_eq!(parsed.parent_hash, Hash256::new([0x11; 32]));
        assert_eq!(parsed.tx_hash, Hash256::new([0x22; 32]));
        assert_eq!(parsed.account_hash, Hash256::new([0x33; 32]));
        assert_eq!(parsed.parent_close_time, 1000);
        assert_eq!(parsed.close_time, 1010);
        assert_eq!(parsed.close_time_resolution, 10);
        assert_eq!(parsed.close_flags, 0);
        assert_eq!(parsed.hash, h.compute_hash());
    }

    #[test]
    fn from_raw_bytes_wrong_size() {
        assert!(LedgerHeader::from_raw_bytes(&[0u8; 100]).is_none());
        assert!(LedgerHeader::from_raw_bytes(&[0u8; 200]).is_none());
        assert!(LedgerHeader::from_raw_bytes(&[0u8; 512]).is_none());
    }
}
