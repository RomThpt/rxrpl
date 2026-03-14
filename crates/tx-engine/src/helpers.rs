use rxrpl_protocol::TransactionResult;
use serde_json::Value;

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
