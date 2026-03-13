pub mod definitions;
pub mod field;
pub mod field_id;
pub mod parser;
pub mod serializer;

use serde_json::Value;

use crate::error::CodecError;
use rxrpl_crypto::hash_prefix::HashPrefix;

/// Encode a JSON transaction/object to XRPL binary format (hex string).
pub fn encode(json: &Value) -> Result<Vec<u8>, CodecError> {
    let mut s = serializer::BinarySerializer::new();
    s.serialize_object(json, true)?;
    Ok(s.into_bytes())
}

/// Decode XRPL binary format to JSON.
pub fn decode(bytes: &[u8]) -> Result<Value, CodecError> {
    let mut p = parser::BinaryParser::new(bytes);
    p.parse_object()
}

/// Encode for signing: prepend STX prefix, skip non-signing fields.
pub fn encode_for_signing(json: &Value) -> Result<Vec<u8>, CodecError> {
    let prefix = HashPrefix::TX_SIGN.to_bytes();
    let mut s = serializer::BinarySerializer::new();
    s.write_bytes(&prefix);
    s.serialize_object(json, false)?;
    Ok(s.into_bytes())
}

/// Encode for multi-signing: prepend SMT prefix, skip non-signing fields,
/// set SigningPubKey to empty, append account ID.
pub fn encode_for_multisigning(json: &Value, account_id: &[u8; 20]) -> Result<Vec<u8>, CodecError> {
    let prefix = HashPrefix::TX_MULTI_SIGN.to_bytes();
    let mut modified = json.clone();
    if let Some(obj) = modified.as_object_mut() {
        obj.insert("SigningPubKey".to_string(), Value::String(String::new()));
    }
    let mut s = serializer::BinarySerializer::new();
    s.write_bytes(&prefix);
    s.serialize_object(&modified, false)?;
    s.write_bytes(account_id);
    Ok(s.into_bytes())
}
