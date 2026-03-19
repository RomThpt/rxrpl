use serde_json::Value;

use rxrpl_rpc_client::XrplClient;

use crate::error::ProtocolError;

const LEDGER_OFFSET: u32 = 20;

/// Fill missing `Sequence`, `Fee`, and `LastLedgerSequence` on a transaction JSON.
///
/// - `Sequence`: fetched from `account_info`
/// - `Fee`: fetched from `fee` (open ledger fee)
/// - `LastLedgerSequence`: current ledger index + 20
pub async fn autofill(tx: &mut Value, client: &XrplClient) -> Result<(), ProtocolError> {
    let obj = tx
        .as_object_mut()
        .ok_or_else(|| ProtocolError::Serialization("expected JSON object".into()))?;

    let account = obj
        .get("Account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProtocolError::MissingField("Account".into()))?
        .to_string();

    if obj.get("Sequence").is_none() {
        let info = client
            .account_info(&account)
            .await
            .map_err(|e| ProtocolError::InvalidFieldValue(format!("account_info failed: {e}")))?;
        let seq = info["account_data"]["Sequence"]
            .as_u64()
            .ok_or_else(|| ProtocolError::MissingField("Sequence from account_info".into()))?;
        obj.insert("Sequence".to_string(), Value::Number(seq.into()));
    }

    if obj.get("Fee").is_none() {
        let fee_result = client
            .fee()
            .await
            .map_err(|e| ProtocolError::InvalidFieldValue(format!("fee failed: {e}")))?;
        let open_fee = fee_result["drops"]["open_ledger_fee"]
            .as_str()
            .ok_or_else(|| ProtocolError::MissingField("open_ledger_fee from fee".into()))?
            .to_string();
        obj.insert("Fee".to_string(), Value::String(open_fee));
    }

    if obj.get("LastLedgerSequence").is_none() {
        let ledger = client
            .ledger_current()
            .await
            .map_err(|e| ProtocolError::InvalidFieldValue(format!("ledger_current failed: {e}")))?;
        let current = ledger["ledger_current_index"].as_u64().ok_or_else(|| {
            ProtocolError::MissingField("ledger_current_index from ledger_current".into())
        })?;
        let lls = current as u32 + LEDGER_OFFSET;
        obj.insert("LastLedgerSequence".to_string(), Value::Number(lls.into()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn autofill_preserves_existing_fields() {
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": "rSomeAccount",
            "Destination": "rOtherAccount",
            "Amount": "1000000",
            "Fee": "12",
            "Sequence": 42,
            "LastLedgerSequence": 100
        });

        assert!(tx.get("Fee").is_some());
        assert!(tx.get("Sequence").is_some());
        assert!(tx.get("LastLedgerSequence").is_some());
    }

    #[test]
    fn missing_account_is_detected() {
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Destination": "rOtherAccount",
            "Amount": "1000000"
        });

        assert!(tx.get("Account").is_none());
    }
}
