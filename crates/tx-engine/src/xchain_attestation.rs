//! Shared logic for the XChain attestation transactors (XChainAddClaimAttestation,
//! XChainAddAccountCreateAttestation, XChainClaim).
//!
//! Mirrors rippled's `XChainBridge.cpp` helpers: `getSignersListAndQuorum`,
//! `transferHelper`, and `finalizeClaimHelper`. Only the XRP path is byte-exact
//! verified (oracle ledger 59); IOU payouts are not implemented.

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::owner_dir;
use crate::view::apply_view::ApplyView;
use crate::view::read_view::ReadView;

/// Verify the witness signature over the canonical attestation field bag.
/// The outer transaction signature authenticates the submitter, not the witness.
pub fn verify_attestation_proof(tx: &Value, create_account: bool) -> Result<(), TransactionResult> {
    let public_key = attestation_public_key(tx)?;
    let signature = hex::decode(
        helpers::get_str_field(tx, "Signature").ok_or(TransactionResult::TemMalformed)?,
    )
    .map_err(|_| TransactionResult::TemXChainBadProof)?;
    let amount = helpers::get_xrp_amount(tx).ok_or(TransactionResult::TemXChainBadProof)?;
    let was_locking =
        helpers::get_u32_field(tx, "WasLockingChainSend").ok_or(TransactionResult::TemMalformed)?;
    if amount == 0 || was_locking > 1 {
        return Err(TransactionResult::TemXChainBadProof);
    }
    let issue = &tx["XChainBridge"][if was_locking == 1 {
        "LockingChainIssue"
    } else {
        "IssuingChainIssue"
    }];
    if issue.as_str() != Some("XRP") && issue.get("currency").and_then(Value::as_str) != Some("XRP")
    {
        return Err(TransactionResult::TemXChainBadProof);
    }

    // XChainAttestations.cpp serializes this STObject without a hash prefix.
    let mut message = serde_json::Map::new();
    for field in [
        "XChainBridge",
        "Amount",
        "OtherChainSource",
        "AttestationRewardAccount",
        "WasLockingChainSend",
    ] {
        message.insert(
            field.into(),
            tx.get(field)
                .ok_or(TransactionResult::TemMalformed)?
                .clone(),
        );
    }
    let counter = if create_account {
        "XChainAccountCreateCount"
    } else {
        "XChainClaimID"
    };
    message.insert(
        counter.into(),
        tx.get(counter)
            .ok_or(TransactionResult::TemMalformed)?
            .clone(),
    );
    if let Some(dst) = tx.get("Destination") {
        message.insert("Destination".into(), dst.clone());
    } else if create_account {
        return Err(TransactionResult::TemMalformed);
    }
    if create_account {
        message.insert(
            "SignatureReward".into(),
            tx.get("SignatureReward")
                .ok_or(TransactionResult::TemMalformed)?
                .clone(),
        );
    }
    let bytes = rxrpl_codec::binary::encode(&Value::Object(message))
        .map_err(|_| TransactionResult::TemMalformed)?;
    if !rxrpl_crypto::verify_signature(&public_key, &bytes, &signature) {
        return Err(TransactionResult::TemXChainBadProof);
    }
    Ok(())
}

fn attestation_public_key(tx: &Value) -> Result<Vec<u8>, TransactionResult> {
    let key = hex::decode(
        helpers::get_str_field(tx, "PublicKey").ok_or(TransactionResult::TemMalformed)?,
    )
    .map_err(|_| TransactionResult::TemMalformed)?;
    if key.len() != 33 || !matches!(key[0], 0x02 | 0x03 | 0xED) {
        return Err(TransactionResult::TemMalformed);
    }
    Ok(key)
}

/// Require a current witness's enabled master key or configured regular key.
pub fn check_attestation_signer(view: &dyn ReadView, tx: &Value) -> Result<(), TransactionResult> {
    let bridge = tx
        .get("XChainBridge")
        .ok_or(TransactionResult::TemXChainBridge)?;
    let bridge_key = crate::bridge_helpers::find_bridge_keylet(bridge, |k| view.exists(k))?;
    let bridge_sle: Value = serde_json::from_slice(
        &view
            .read(&bridge_key)
            .ok_or(TransactionResult::TecNoEntry)?,
    )
    .map_err(|_| TransactionResult::TefInternal)?;
    let door = bridge_sle["Account"]
        .as_str()
        .ok_or(TransactionResult::TefInternal)?;
    let door_is_locking = bridge["LockingChainDoor"].as_str() == Some(door);
    if tx["WasLockingChainSend"].as_u64() != Some(u64::from(!door_is_locking)) {
        return Err(TransactionResult::TecXChainWrongChain);
    }
    let (signers, _) = read_signers_and_quorum(view, door)?;
    let signer = helpers::get_str_field(tx, "AttestationSignerAccount")
        .ok_or(TransactionResult::TemMalformed)?;
    if !signers.contains_key(signer) {
        return Err(TransactionResult::TecNoPermission);
    }
    let signer_id =
        decode_account_id(signer).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let key_id =
        rxrpl_codec::address::classic::account_id_from_public_key(&attestation_public_key(tx)?);
    let account = view.read(&keylet::account(&signer_id));
    let account: Option<Value> = account
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|_| TransactionResult::TefInternal)?;
    if key_id == signer_id {
        if account
            .as_ref()
            .and_then(|v| v["Flags"].as_u64())
            .unwrap_or(0)
            & 0x0010_0000
            != 0
        {
            return Err(TransactionResult::TecXChainBadPublicKeyAccountPair);
        }
    } else {
        let regular_key = account
            .as_ref()
            .and_then(|v| v["RegularKey"].as_str())
            .and_then(|s| decode_account_id(s).ok());
        if regular_key != Some(key_id) {
            return Err(TransactionResult::TecXChainBadPublicKeyAccountPair);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// The door account's witness signer list and the quorum threshold, derived from
/// the door's `SignerList` SLE (`getSignersListAndQuorum`, XChainBridge.cpp:748).
/// Quorum is the `SignerQuorum` field; weights are per-entry `SignerWeight`.
pub fn read_signers_and_quorum(
    view: &dyn ReadView,
    door_str: &str,
) -> Result<(std::collections::BTreeMap<String, u64>, u64), TransactionResult> {
    let door_id =
        decode_account_id(door_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let sl_bytes = view
        .read(&keylet::signer_list(&door_id))
        .ok_or(TransactionResult::TecXChainNoSignersList)?;
    let sl: Value =
        serde_json::from_slice(&sl_bytes).map_err(|_| TransactionResult::TefInternal)?;
    let quorum = sl.get("SignerQuorum").and_then(|v| v.as_u64()).unwrap_or(0);
    let mut signers = std::collections::BTreeMap::new();
    if let Some(entries) = sl.get("SignerEntries").and_then(|v| v.as_array()) {
        for e in entries {
            let entry = e.get("SignerEntry").unwrap_or(e);
            if let (Some(acct), Some(weight)) = (
                entry.get("Account").and_then(|v| v.as_str()),
                entry.get("SignerWeight").and_then(|v| v.as_u64()),
            ) {
                signers.insert(acct.to_string(), weight);
            }
        }
    }
    Ok((signers, quorum))
}

/// Add `amount` drops to an account's XRP balance.
fn credit_xrp(
    view: &mut dyn ApplyView,
    acct_str: &str,
    amount: u64,
) -> Result<(), TransactionResult> {
    let id = decode_account_id(acct_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let key = keylet::account(&id);
    let bytes = view.read(&key).ok_or(TransactionResult::TecNoDst)?;
    let mut acct: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let bal = helpers::get_balance(&acct);
    helpers::set_balance(&mut acct, bal + amount);
    view.update(
        key,
        serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?,
    )
    .map_err(|_| TransactionResult::TefInternal)
}

/// Credit `amount` to `acct_str`, creating its AccountRoot (Sequence =
/// `ledger_seq`) if it does not yet exist — mirroring `transferHelper`'s
/// account-creation branch on the XRP path.
fn create_or_credit_xrp(
    view: &mut dyn ApplyView,
    acct_str: &str,
    amount: u64,
    ledger_seq: u32,
) -> Result<(), TransactionResult> {
    let id = decode_account_id(acct_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let key = keylet::account(&id);
    if view.read(&key).is_some() {
        return credit_xrp(view, acct_str, amount);
    }
    // rippled's transferHelper account-create sets only Account, Balance and
    // Sequence; OwnerCount and Flags are left unset (absent, default zero).
    let seq = ledger_seq.max(1);
    let new_account = serde_json::json!({
        "LedgerEntryType": "AccountRoot",
        "Account": acct_str,
        "Balance": amount.to_string(),
        "Sequence": seq,
        "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
        "PreviousTxnLgrSeq": seq,
    });
    view.insert(
        key,
        serde_json::to_vec(&new_account).map_err(|_| TransactionResult::TefInternal)?,
    )
    .map_err(|_| TransactionResult::TefInternal)
}

/// Subtract `amount` drops from an account's XRP balance.
fn debit_xrp(
    view: &mut dyn ApplyView,
    acct_str: &str,
    amount: u64,
) -> Result<(), TransactionResult> {
    let id = decode_account_id(acct_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let key = keylet::account(&id);
    let bytes = view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
    let mut acct: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let bal = helpers::get_balance(&acct);
    if bal < amount {
        return Err(TransactionResult::TecUnfundedPayment);
    }
    helpers::set_balance(&mut acct, bal - amount);
    view.update(
        key,
        serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?,
    )
    .map_err(|_| TransactionResult::TefInternal)
}

/// `transferHelper` for the XRP path: move `amount` drops from `src` to `dst`.
/// A transfer to self is a no-op (XChainBridge.cpp:403).
fn transfer_xrp(
    view: &mut dyn ApplyView,
    src: &str,
    dst: &str,
    amount: u64,
) -> Result<(), TransactionResult> {
    if src == dst {
        return Ok(());
    }
    debit_xrp(view, src, amount)?;
    credit_xrp(view, dst, amount)
}

/// `finalizeClaimHelper` for the XRP path (XChainBridge.cpp:595). Pays the main
/// funds from the destination-chain door to `dst`, splits the reward pool among
/// `reward_accounts`, then deletes the claim-id SLE, removes it from the owner's
/// directory, and decrements the owner's owner count.
///
/// `door` is the paying door (the bridge SLE's `Account`); `claim_owner` is the
/// claim-id SLE's `Account`, which is also the reward-pool source.
#[allow(clippy::too_many_arguments)]
pub fn finalize_claim_xrp(
    view: &mut dyn ApplyView,
    door: &str,
    dst: &str,
    claim_owner: &str,
    amount: u64,
    reward_pool: u64,
    reward_accounts: &[String],
    claim_key: &rxrpl_primitives::Hash256,
) -> Result<(), TransactionResult> {
    transfer_xrp(view, door, dst, amount)?;

    if !reward_accounts.is_empty() {
        let share = reward_pool / reward_accounts.len() as u64;
        for ra in reward_accounts {
            transfer_xrp(view, claim_owner, ra, share)?;
        }
    }

    remove_claim(view, claim_owner, claim_key)
}

/// Release an existing claim and its owner's directory entry and reserve.
pub fn remove_claim(
    view: &mut dyn ApplyView,
    claim_owner: &str,
    claim_key: &rxrpl_primitives::Hash256,
) -> Result<(), TransactionResult> {
    view.erase(claim_key)
        .map_err(|_| TransactionResult::TefInternal)?;
    let owner_id =
        decode_account_id(claim_owner).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    owner_dir::remove_from_owner_dir_keep_root(view, &owner_id, claim_key)?;

    let owner_key = keylet::account(&owner_id);
    let owner_bytes = view
        .read(&owner_key)
        .ok_or(TransactionResult::TerNoAccount)?;
    let mut owner: Value =
        serde_json::from_slice(&owner_bytes).map_err(|_| TransactionResult::TefInternal)?;
    helpers::adjust_owner_count(&mut owner, -1);
    view.update(
        owner_key,
        serde_json::to_vec(&owner).map_err(|_| TransactionResult::TefInternal)?,
    )
    .map_err(|_| TransactionResult::TefInternal)?;

    Ok(())
}

/// `finalizeClaimHelper` for the create-account variant (XChainBridge.cpp:1090):
/// the door funds the new account (creating it) and pays the reward pool, both
/// from the door. No claim-id object survives the in-order finalize path, so the
/// caller advances the bridge `XChainAccountClaimCount` afterwards.
#[allow(clippy::too_many_arguments)]
pub fn finalize_create_account_xrp(
    view: &mut dyn ApplyView,
    door: &str,
    dst: &str,
    amount: u64,
    reward_pool: u64,
    reward_accounts: &[String],
    ledger_seq: u32,
) -> Result<(), TransactionResult> {
    debit_xrp(view, door, amount)?;
    create_or_credit_xrp(view, dst, amount, ledger_seq)?;

    if !reward_accounts.is_empty() {
        let share = reward_pool / reward_accounts.len() as u64;
        for ra in reward_accounts {
            transfer_xrp(view, door, ra, share)?;
        }
    }

    Ok(())
}
