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

/// Extract the raw value bytes of the field `(type_code, field_code)` from a
/// serialized STObject, or `None` if the field is absent.
pub fn extract_field(
    data: &[u8],
    type_code: i32,
    field_code: i32,
) -> Result<Option<Vec<u8>>, CodecError> {
    parser::BinaryParser::new(data).extract_field_value(type_code, field_code)
}

/// Decode XRPL binary format to JSON.
pub fn decode(bytes: &[u8]) -> Result<Value, CodecError> {
    let mut p = parser::BinaryParser::new(bytes);
    p.parse_object()
}

/// Prefix `bytes` with their XRPL variable-length encoding. Used to build the
/// transaction+metadata SHAMap leaf, whose content is `VL(tx) || VL(meta)`.
pub fn encode_vl(bytes: &[u8]) -> Vec<u8> {
    let len = bytes.len();
    let mut out = Vec::with_capacity(len + 3);
    if len <= 192 {
        out.push(len as u8);
    } else if len <= 12480 {
        let a = len - 193;
        out.push((a >> 8) as u8 + 193);
        out.push((a & 0xFF) as u8);
    } else {
        let a = len - 12481;
        out.push(241 + (a >> 16) as u8);
        out.push(((a >> 8) & 0xFF) as u8);
        out.push((a & 0xFF) as u8);
    }
    out.extend_from_slice(bytes);
    out
}

/// Split a transaction+metadata SHAMap leaf (`VL(tx) || VL(meta)`) back into the
/// decoded transaction and metadata JSON values.
pub fn decode_tx_leaf(data: &[u8]) -> Result<(Value, Value), CodecError> {
    let mut p = parser::BinaryParser::new(data);
    let tx_bytes = p.read_vl_blob()?;
    let meta_bytes = p.read_vl_blob()?;
    Ok((decode(&tx_bytes)?, decode(&meta_bytes)?))
}

/// Decode a transaction SHAMap leaf into the `{tx_json, meta}` record shape
/// consumed by the RPC handlers and event emitters.
pub fn decode_tx_record(data: &[u8]) -> Result<Value, CodecError> {
    let (tx, meta) = decode_tx_leaf(data)?;
    Ok(serde_json::json!({ "tx_json": tx, "meta": meta }))
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
