//! Binary SLE codec for XRPL ledger entries.
//!
//! Provides encode/decode functions to convert ledger entries between
//! JSON bytes (used internally by handlers) and XRPL binary format
//! (used in SHAMaps for hash compatibility with rippled).

use rxrpl_codec::binary;
use rxrpl_codec::CodecError;
use serde_json::Value;

/// Encode a JSON ledger entry to XRPL binary format.
///
/// Parses `json_bytes` as JSON, then serializes to canonical XRPL binary.
/// Falls back to storing the original bytes if:
/// - The input is not valid JSON (raw data from tests)
/// - The binary codec does not yet support all fields in this entry type
pub fn encode_sle(json_bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    let value: Value = match serde_json::from_slice(json_bytes) {
        Ok(v) => v,
        Err(_) => return Ok(json_bytes.to_vec()),
    };
    match binary::encode(&value) {
        Ok(binary) => Ok(binary),
        Err(_) => Ok(json_bytes.to_vec()),
    }
}

/// Decode XRPL binary to JSON bytes.
///
/// If data starts with `{` or `[`, it is already JSON. Otherwise parses
/// as XRPL binary and serializes back to JSON bytes.
pub fn decode_sle(binary_bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
    if matches!(binary_bytes.first(), Some(b'{') | Some(b'[')) {
        return Ok(binary_bytes.to_vec());
    }
    let value = binary::decode(binary_bytes)?;
    serde_json::to_vec(&value).map_err(CodecError::Json)
}

/// Decode raw state bytes (binary or JSON) to a JSON [`Value`].
///
/// If data starts with `{` or `[`, it is JSON (XRPL binary never starts
/// with those bytes). Otherwise tries binary decode, falls back to JSON.
pub fn decode_state(bytes: &[u8]) -> Result<Value, CodecError> {
    if matches!(bytes.first(), Some(b'{') | Some(b'[')) {
        return serde_json::from_slice(bytes).map_err(CodecError::Json);
    }
    match binary::decode(bytes) {
        Ok(value) => Ok(value),
        Err(_) => serde_json::from_slice(bytes).map_err(CodecError::Json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_account_root() {
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Balance": "100000000000000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let json_bytes = serde_json::to_vec(&account).unwrap();
        let binary = encode_sle(&json_bytes).unwrap();
        let decoded_bytes = decode_sle(&binary).unwrap();
        let decoded: Value = serde_json::from_slice(&decoded_bytes).unwrap();

        assert_eq!(decoded["LedgerEntryType"], "AccountRoot");
        assert_eq!(decoded["Account"], "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh");
        assert_eq!(decoded["Balance"], "100000000000000000");
        assert_eq!(decoded["Sequence"], 1);
        assert_eq!(decoded["OwnerCount"], 0);
        assert_eq!(decoded["Flags"], 0);
    }

    #[test]
    fn round_trip_offer() {
        let offer = serde_json::json!({
            "LedgerEntryType": "Offer",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Sequence": 5,
            "TakerPays": "1000000",
            "TakerGets": "500000",
            "Flags": 0,
        });
        let json_bytes = serde_json::to_vec(&offer).unwrap();
        let binary = encode_sle(&json_bytes).unwrap();
        let decoded_bytes = decode_sle(&binary).unwrap();
        let decoded: Value = serde_json::from_slice(&decoded_bytes).unwrap();

        assert_eq!(decoded["LedgerEntryType"], "Offer");
        assert_eq!(decoded["Sequence"], 5);
    }

    #[test]
    fn round_trip_fee_settings() {
        let fee = serde_json::json!({
            "LedgerEntryType": "FeeSettings",
            "BaseFee": "a",
            "ReferenceFeeUnits": 10,
            "ReserveBase": 10000000,
            "ReserveIncrement": 2000000,
            "Flags": 0,
        });
        let json_bytes = serde_json::to_vec(&fee).unwrap();
        let binary = encode_sle(&json_bytes).unwrap();
        let decoded_bytes = decode_sle(&binary).unwrap();
        let decoded: Value = serde_json::from_slice(&decoded_bytes).unwrap();

        assert_eq!(decoded["LedgerEntryType"], "FeeSettings");
    }

    #[test]
    fn decode_state_binary() {
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let json_bytes = serde_json::to_vec(&account).unwrap();
        let binary = encode_sle(&json_bytes).unwrap();

        // decode_state should handle binary input
        let value = decode_state(&binary).unwrap();
        assert_eq!(value["LedgerEntryType"], "AccountRoot");
    }

    #[test]
    fn decode_state_json_fallback() {
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let json_bytes = serde_json::to_vec(&account).unwrap();

        // decode_state should fall back to JSON
        let value = decode_state(&json_bytes).unwrap();
        assert_eq!(value["LedgerEntryType"], "AccountRoot");
    }

    #[test]
    fn encode_deterministic() {
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Balance": "100000000000000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let json_bytes = serde_json::to_vec(&account).unwrap();
        let binary1 = encode_sle(&json_bytes).unwrap();
        let binary2 = encode_sle(&json_bytes).unwrap();
        assert_eq!(binary1, binary2, "encoding must be deterministic");
    }
}
