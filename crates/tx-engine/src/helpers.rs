use rxrpl_protocol::TransactionResult;
use serde_json::Value;

/// Extract an IOU Amount object from a transaction.
/// Returns (currency, issuer, value) if the Amount field is an object.
pub fn get_iou_amount<'a>(tx: &'a Value) -> Option<(&'a str, &'a str, &'a str)> {
    let amount = tx.get("Amount")?;
    if !amount.is_object() {
        return None;
    }
    let currency = amount.get("currency")?.as_str()?;
    let issuer = amount.get("issuer")?.as_str()?;
    let value = amount.get("value")?.as_str()?;
    Some((currency, issuer, value))
}

/// Access an array field from a transaction JSON.
pub fn get_array_field<'a>(tx: &'a Value, field: &str) -> Option<&'a Vec<Value>> {
    tx.get(field).and_then(|v| v.as_array())
}

/// Convert a 3-letter currency code (or 40-char hex) to 20 bytes.
pub fn currency_to_bytes(currency: &str) -> [u8; 20] {
    let mut bytes = [0u8; 20];
    let code = currency.as_bytes();
    if code.len() == 3 {
        bytes[12] = code[0];
        bytes[13] = code[1];
        bytes[14] = code[2];
    } else if code.len() == 40 {
        if let Ok(decoded) = hex::decode(currency) {
            if decoded.len() == 20 {
                bytes.copy_from_slice(&decoded);
            }
        }
    }
    bytes
}

/// Extract the "Account" field from a transaction JSON.
pub fn get_account(tx: &Value) -> Result<&str, TransactionResult> {
    tx.get("Account")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TemMalformed)
}

/// Extract the "Destination" field from a transaction JSON.
pub fn get_destination(tx: &Value) -> Result<&str, TransactionResult> {
    tx.get("Destination")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TemDstIsObligatory)
}

/// Extract the "Fee" field as drops (u64).
pub fn get_fee(tx: &Value) -> u64 {
    tx.get("Fee")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Extract the "Amount" field as XRP drops (u64).
/// Returns None if Amount is an IOU object.
pub fn get_xrp_amount(tx: &Value) -> Option<u64> {
    tx.get("Amount")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
}

/// Get the balance from an AccountRoot JSON object as drops.
pub fn get_balance(account_obj: &Value) -> u64 {
    account_obj["Balance"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Set the balance on an AccountRoot JSON object.
pub fn set_balance(account_obj: &mut Value, balance: u64) {
    account_obj["Balance"] = Value::String(balance.to_string());
}

/// Get the sequence number from an AccountRoot.
pub fn get_sequence(account_obj: &Value) -> u32 {
    account_obj["Sequence"].as_u64().unwrap_or(0) as u32
}

/// Increment the sequence number on an AccountRoot.
pub fn increment_sequence(account_obj: &mut Value) {
    let seq = get_sequence(account_obj);
    account_obj["Sequence"] = Value::from(seq + 1);
}

/// Get the flags from a ledger object.
pub fn get_flags(obj: &Value) -> u32 {
    obj["Flags"].as_u64().unwrap_or(0) as u32
}

/// Get the owner count from an AccountRoot.
pub fn get_owner_count(account_obj: &Value) -> u32 {
    account_obj["OwnerCount"].as_u64().unwrap_or(0) as u32
}

/// Adjust the owner count on an AccountRoot.
pub fn adjust_owner_count(account_obj: &mut Value, delta: i32) {
    let current = get_owner_count(account_obj) as i32;
    let new_count = (current + delta).max(0) as u32;
    account_obj["OwnerCount"] = Value::from(new_count);
}

/// Extract a string field from a transaction JSON.
pub fn get_str_field<'a>(tx: &'a Value, field: &str) -> Option<&'a str> {
    tx.get(field).and_then(|v| v.as_str())
}

/// Extract a u32 field from a transaction JSON.
pub fn get_u32_field(tx: &Value, field: &str) -> Option<u32> {
    tx.get(field).and_then(|v| v.as_u64()).map(|n| n as u32)
}

/// Extract a u64 field (stored as string) from a transaction JSON.
pub fn get_u64_str_field(tx: &Value, field: &str) -> Option<u64> {
    tx.get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
}

/// Look up an AccountRoot by address and return the keylet + parsed JSON.
pub fn read_account_by_address(
    view: &dyn crate::view::read_view::ReadView,
    address: &str,
) -> Result<(rxrpl_primitives::Hash256, Value), TransactionResult> {
    let account_id = rxrpl_codec::address::classic::decode_account_id(address)
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let key = rxrpl_protocol::keylet::account(&account_id);
    let bytes = view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
    let obj: Value = serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    Ok((key, obj))
}
