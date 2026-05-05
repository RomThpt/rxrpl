//! Shard exchange protocol messages.
//!
//! Binary wire format (custom, not protobuf) for shard-related P2P messages.
//! Each message is serialized as a sequence of length-prefixed fields using
//! big-endian byte order, matching the conventions in the overlay codec.

/// Request shard availability from a peer. No fields needed.
#[derive(Clone, Debug, Default)]
pub struct TMGetShards;

/// Response with available shard indices.
#[derive(Clone, Debug, Default)]
pub struct TMShards {
    /// Indices of complete shards held by the peer.
    pub indices: Vec<u32>,
    /// `(index, stored_count)` pairs for incomplete shards.
    pub incomplete: Vec<(u32, u32)>,
}

/// Request specific ledger data from a shard.
#[derive(Clone, Debug)]
pub struct TMGetShardData {
    /// The shard index being requested.
    pub shard_index: u32,
    /// Specific ledger sequences requested within the shard.
    pub ledger_seqs: Vec<u32>,
}

/// Response with shard ledger data.
#[derive(Clone, Debug)]
pub struct TMShardData {
    /// The shard index this data belongs to.
    pub shard_index: u32,
    /// Ledger entries contained in the response.
    pub ledgers: Vec<ShardLedgerEntry>,
}

/// A single ledger entry within a shard data response.
#[derive(Clone, Debug)]
pub struct ShardLedgerEntry {
    /// Ledger sequence number.
    pub seq: u32,
    /// Ledger hash (32 bytes).
    pub hash: [u8; 32],
    /// Raw ledger data.
    pub data: Vec<u8>,
}

// --- Wire encoding/decoding ---
//
// Format overview:
//   TMGetShards:    empty payload
//   TMShards:       [u32 complete_count] [u32...]  [u32 incomplete_count] [(u32, u32)...]
//   TMGetShardData: [u32 shard_index] [u32 seq_count] [u32...]
//   TMShardData:    [u32 shard_index] [u32 entry_count] [entry...]
//     entry:        [u32 seq] [32-byte hash] [u32 data_len] [data bytes]

/// Encode a `TMGetShards` message (empty payload).
pub fn encode_get_shards() -> Vec<u8> {
    Vec::new()
}

/// Decode a `TMGetShards` message.
pub fn decode_get_shards(_data: &[u8]) -> Result<TMGetShards, String> {
    Ok(TMGetShards)
}

/// Encode a `TMShards` message.
pub fn encode_shards(msg: &TMShards) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + msg.indices.len() * 4 + msg.incomplete.len() * 8);

    // Complete shard count + indices
    buf.extend_from_slice(&(msg.indices.len() as u32).to_be_bytes());
    for &idx in &msg.indices {
        buf.extend_from_slice(&idx.to_be_bytes());
    }

    // Incomplete shard count + (index, stored_count) pairs
    buf.extend_from_slice(&(msg.incomplete.len() as u32).to_be_bytes());
    for &(idx, count) in &msg.incomplete {
        buf.extend_from_slice(&idx.to_be_bytes());
        buf.extend_from_slice(&count.to_be_bytes());
    }

    buf
}

/// Decode a `TMShards` message.
pub fn decode_shards(data: &[u8]) -> Result<TMShards, String> {
    let mut pos = 0;

    let complete_count = read_u32(data, &mut pos)?;
    let mut indices = Vec::with_capacity(complete_count as usize);
    for _ in 0..complete_count {
        indices.push(read_u32(data, &mut pos)?);
    }

    let incomplete_count = read_u32(data, &mut pos)?;
    let mut incomplete = Vec::with_capacity(incomplete_count as usize);
    for _ in 0..incomplete_count {
        let idx = read_u32(data, &mut pos)?;
        let count = read_u32(data, &mut pos)?;
        incomplete.push((idx, count));
    }

    Ok(TMShards {
        indices,
        incomplete,
    })
}

/// Encode a `TMGetShardData` message.
pub fn encode_get_shard_data(msg: &TMGetShardData) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + msg.ledger_seqs.len() * 4);

    buf.extend_from_slice(&msg.shard_index.to_be_bytes());
    buf.extend_from_slice(&(msg.ledger_seqs.len() as u32).to_be_bytes());
    for &seq in &msg.ledger_seqs {
        buf.extend_from_slice(&seq.to_be_bytes());
    }

    buf
}

/// Decode a `TMGetShardData` message.
pub fn decode_get_shard_data(data: &[u8]) -> Result<TMGetShardData, String> {
    let mut pos = 0;

    let shard_index = read_u32(data, &mut pos)?;
    let seq_count = read_u32(data, &mut pos)?;
    let mut ledger_seqs = Vec::with_capacity(seq_count as usize);
    for _ in 0..seq_count {
        ledger_seqs.push(read_u32(data, &mut pos)?);
    }

    Ok(TMGetShardData {
        shard_index,
        ledger_seqs,
    })
}

/// Encode a `TMShardData` message.
pub fn encode_shard_data(msg: &TMShardData) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + msg.ledgers.len() * 40);

    buf.extend_from_slice(&msg.shard_index.to_be_bytes());
    buf.extend_from_slice(&(msg.ledgers.len() as u32).to_be_bytes());
    for entry in &msg.ledgers {
        buf.extend_from_slice(&entry.seq.to_be_bytes());
        buf.extend_from_slice(&entry.hash);
        buf.extend_from_slice(&(entry.data.len() as u32).to_be_bytes());
        buf.extend_from_slice(&entry.data);
    }

    buf
}

/// Decode a `TMShardData` message.
pub fn decode_shard_data(data: &[u8]) -> Result<TMShardData, String> {
    let mut pos = 0;

    let shard_index = read_u32(data, &mut pos)?;
    let entry_count = read_u32(data, &mut pos)?;
    let mut ledgers = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        let seq = read_u32(data, &mut pos)?;
        let hash = read_hash(data, &mut pos)?;
        let data_len = read_u32(data, &mut pos)? as usize;
        if pos + data_len > data.len() {
            return Err("truncated shard data entry".into());
        }
        let entry_data = data[pos..pos + data_len].to_vec();
        pos += data_len;
        ledgers.push(ShardLedgerEntry {
            seq,
            hash,
            data: entry_data,
        });
    }

    Ok(TMShardData {
        shard_index,
        ledgers,
    })
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, String> {
    if *pos + 4 > data.len() {
        return Err("truncated message: expected u32".into());
    }
    let val = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(val)
}

fn read_hash(data: &[u8], pos: &mut usize) -> Result<[u8; 32], String> {
    if *pos + 32 > data.len() {
        return Err("truncated message: expected 32-byte hash".into());
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&data[*pos..*pos + 32]);
    *pos += 32;
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_shards_roundtrip() {
        let encoded = encode_get_shards();
        assert!(encoded.is_empty());
        let decoded = decode_get_shards(&encoded).unwrap();
        let _ = decoded; // TMGetShards has no fields
    }

    #[test]
    fn shards_roundtrip() {
        let msg = TMShards {
            indices: vec![0, 3, 7],
            incomplete: vec![(1, 500), (5, 12000)],
        };
        let encoded = encode_shards(&msg);
        let decoded = decode_shards(&encoded).unwrap();
        assert_eq!(decoded.indices, msg.indices);
        assert_eq!(decoded.incomplete, msg.incomplete);
    }

    #[test]
    fn shards_empty_roundtrip() {
        let msg = TMShards::default();
        let encoded = encode_shards(&msg);
        let decoded = decode_shards(&encoded).unwrap();
        assert!(decoded.indices.is_empty());
        assert!(decoded.incomplete.is_empty());
    }

    #[test]
    fn get_shard_data_roundtrip() {
        let msg = TMGetShardData {
            shard_index: 5,
            ledger_seqs: vec![81920, 81921, 81922],
        };
        let encoded = encode_get_shard_data(&msg);
        let decoded = decode_get_shard_data(&encoded).unwrap();
        assert_eq!(decoded.shard_index, 5);
        assert_eq!(decoded.ledger_seqs, msg.ledger_seqs);
    }

    #[test]
    fn shard_data_roundtrip() {
        let msg = TMShardData {
            shard_index: 2,
            ledgers: vec![
                ShardLedgerEntry {
                    seq: 32768,
                    hash: [0xAA; 32],
                    data: vec![1, 2, 3, 4],
                },
                ShardLedgerEntry {
                    seq: 32769,
                    hash: [0xBB; 32],
                    data: vec![5, 6, 7],
                },
            ],
        };
        let encoded = encode_shard_data(&msg);
        let decoded = decode_shard_data(&encoded).unwrap();
        assert_eq!(decoded.shard_index, 2);
        assert_eq!(decoded.ledgers.len(), 2);
        assert_eq!(decoded.ledgers[0].seq, 32768);
        assert_eq!(decoded.ledgers[0].hash, [0xAA; 32]);
        assert_eq!(decoded.ledgers[0].data, vec![1, 2, 3, 4]);
        assert_eq!(decoded.ledgers[1].seq, 32769);
        assert_eq!(decoded.ledgers[1].hash, [0xBB; 32]);
        assert_eq!(decoded.ledgers[1].data, vec![5, 6, 7]);
    }

    #[test]
    fn shard_data_empty_roundtrip() {
        let msg = TMShardData {
            shard_index: 0,
            ledgers: vec![],
        };
        let encoded = encode_shard_data(&msg);
        let decoded = decode_shard_data(&encoded).unwrap();
        assert_eq!(decoded.shard_index, 0);
        assert!(decoded.ledgers.is_empty());
    }

    #[test]
    fn decode_truncated_shards_fails() {
        let data = [0, 0, 0, 5]; // claims 5 indices but has none
        assert!(decode_shards(&data).is_err());
    }

    #[test]
    fn decode_truncated_shard_data_fails() {
        // shard_index=0, entry_count=1 but no entry data
        let data = [0, 0, 0, 0, 0, 0, 0, 1];
        assert!(decode_shard_data(&data).is_err());
    }
}
