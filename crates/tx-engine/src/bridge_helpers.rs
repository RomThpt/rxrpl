use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

/// rippled `keylet::bridge` hashes the door account and the 20-byte currency of
/// that chain's issue (Indexes.cpp), NOT the whole bridge spec. The chain is
/// chosen by which door the submitting `account` is.
pub fn bridge_keylet(account: &str, bridge: &Value) -> Result<Hash256, TransactionResult> {
    let locking_door = bridge
        .get("LockingChainDoor")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TemXChainBridge)?;
    let (door_str, issue) = if account == locking_door {
        (locking_door, bridge.get("LockingChainIssue"))
    } else {
        let issuing_door = bridge
            .get("IssuingChainDoor")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;
        (issuing_door, bridge.get("IssuingChainIssue"))
    };
    let door = rxrpl_codec::address::classic::decode_account_id(door_str)
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let currency = issue_currency(issue.ok_or(TransactionResult::TemXChainBridge)?);
    Ok(keylet::bridge(&door, &currency))
}

/// The 20-byte currency code of an Issue (XRP = all zero).
fn issue_currency(issue: &Value) -> [u8; 20] {
    let cur = issue
        .as_str()
        .or_else(|| issue.get("currency").and_then(|v| v.as_str()))
        .unwrap_or("XRP");
    if cur == "XRP" {
        [0u8; 20]
    } else {
        crate::helpers::currency_to_bytes(cur)
    }
}

/// Compute the bridge keylet from an already-decoded door `AccountId`.
pub fn bridge_keylet_for_door(door: &AccountId, issue: &Value) -> Hash256 {
    keylet::bridge(door, &issue_currency(issue))
}

/// Serialize a BridgeSpec to bytes for use in keylet computation.
/// BridgeSpec contains: LockingChainDoor, LockingChainIssue, IssuingChainDoor, IssuingChainIssue
pub fn serialize_bridge_spec(bridge: &Value) -> Result<Vec<u8>, TransactionResult> {
    let locking_door = bridge
        .get("LockingChainDoor")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TemXChainBridge)?;
    let locking_issue = bridge
        .get("LockingChainIssue")
        .ok_or(TransactionResult::TemXChainBridge)?;
    let issuing_door = bridge
        .get("IssuingChainDoor")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TemXChainBridge)?;
    let issuing_issue = bridge
        .get("IssuingChainIssue")
        .ok_or(TransactionResult::TemXChainBridge)?;

    let mut data = Vec::new();
    let ld = rxrpl_codec::address::classic::decode_account_id(locking_door)
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
    data.extend_from_slice(ld.as_bytes());

    serialize_issue(&mut data, locking_issue)?;

    let id = rxrpl_codec::address::classic::decode_account_id(issuing_door)
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
    data.extend_from_slice(id.as_bytes());

    serialize_issue(&mut data, issuing_issue)?;

    Ok(data)
}

fn serialize_issue(data: &mut Vec<u8>, issue: &Value) -> Result<(), TransactionResult> {
    if let Some(s) = issue.as_str() {
        if s == "XRP" {
            data.extend_from_slice(&[0u8; 20]); // currency
            data.extend_from_slice(&[0u8; 20]); // issuer
            return Ok(());
        }
        return Err(TransactionResult::TemXChainBridge);
    }
    if issue.is_object() {
        let cur = issue
            .get("currency")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;
        // XRP is represented as {"currency": "XRP"} with no issuer.
        if cur == "XRP" {
            if issue.get("issuer").is_some() {
                return Err(TransactionResult::TemXChainBridge);
            }
            data.extend_from_slice(&[0u8; 20]); // currency
            data.extend_from_slice(&[0u8; 20]); // issuer
            return Ok(());
        }
        let iss = issue
            .get("issuer")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;
        data.extend_from_slice(&crate::helpers::currency_to_bytes(cur));
        let iss_id = rxrpl_codec::address::classic::decode_account_id(iss)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        data.extend_from_slice(iss_id.as_bytes());
        return Ok(());
    }
    Err(TransactionResult::TemXChainBridge)
}

/// Verify attestation structure (no crypto verification, just check fields present).
pub fn verify_attestation_structure(attestation: &Value) -> Result<(), TransactionResult> {
    attestation
        .get("AttestationSignerAccount")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TemMalformed)?;
    attestation
        .get("PublicKey")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TemMalformed)?;
    attestation
        .get("Amount")
        .ok_or(TransactionResult::TemMalformed)?;
    attestation
        .get("AttestationRewardAccount")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TemMalformed)?;
    Ok(())
}

/// Check if quorum is reached. Simple count-based quorum.
pub fn check_quorum(attestations: &[Value], quorum: u32) -> bool {
    attestations.len() as u32 >= quorum
}
