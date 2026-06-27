//! Shared logic for the XChain attestation transactors (XChainAddClaimAttestation,
//! XChainAddAccountCreateAttestation, XChainClaim).
//!
//! Mirrors rippled's `XChainBridge.cpp` helpers: `getSignersListAndQuorum`,
//! `transferHelper`, and `finalizeClaimHelper`. Only the XRP path is byte-exact
//! verified (oracle ledger 59); the IOU path is a faithful port pending an oracle.

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::owner_dir;
use crate::view::apply_view::ApplyView;
use crate::view::read_view::ReadView;

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
fn credit_xrp(view: &mut dyn ApplyView, acct_str: &str, amount: u64) -> Result<(), TransactionResult> {
    let id = decode_account_id(acct_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let key = keylet::account(&id);
    let bytes = view.read(&key).ok_or(TransactionResult::TecNoDst)?;
    let mut acct: Value = serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let bal = helpers::get_balance(&acct);
    helpers::set_balance(&mut acct, bal + amount);
    view.update(key, serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?)
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
    view.insert(key, serde_json::to_vec(&new_account).map_err(|_| TransactionResult::TefInternal)?)
        .map_err(|_| TransactionResult::TefInternal)
}

/// Subtract `amount` drops from an account's XRP balance.
fn debit_xrp(view: &mut dyn ApplyView, acct_str: &str, amount: u64) -> Result<(), TransactionResult> {
    let id = decode_account_id(acct_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let key = keylet::account(&id);
    let bytes = view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
    let mut acct: Value = serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let bal = helpers::get_balance(&acct);
    if bal < amount {
        return Err(TransactionResult::TecUnfundedPayment);
    }
    helpers::set_balance(&mut acct, bal - amount);
    view.update(key, serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?)
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

    view.erase(claim_key).map_err(|_| TransactionResult::TefInternal)?;
    let owner_id =
        decode_account_id(claim_owner).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    owner_dir::remove_from_owner_dir_keep_root(view, &owner_id, claim_key)?;

    let owner_key = keylet::account(&owner_id);
    let owner_bytes = view.read(&owner_key).ok_or(TransactionResult::TerNoAccount)?;
    let mut owner: Value =
        serde_json::from_slice(&owner_bytes).map_err(|_| TransactionResult::TefInternal)?;
    helpers::adjust_owner_count(&mut owner, -1);
    view.update(owner_key, serde_json::to_vec(&owner).map_err(|_| TransactionResult::TefInternal)?)
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
