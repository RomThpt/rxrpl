use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::helpers;
use crate::owner_dir::{add_to_owner_dir, remove_from_owner_dir};
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// `lsfOneOwnerCount`: a SignerList created with MultiSignReserve active counts
/// as a single owner-reserve item.
const LSF_ONE_OWNER_COUNT: u32 = 0x0001_0000;

/// SignerListSet transaction handler.
///
/// Sets, updates, or removes the signer list for multi-signing.
/// If SignerQuorum is 0, removes the signer list.
pub struct SignerListSetTransactor;

impl Transactor for SignerListSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let quorum = ctx
            .tx
            .get("SignerQuorum")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        if quorum == 0 {
            // Deleting the signer list -- no entries should be present
            if ctx
                .tx
                .get("SignerEntries")
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty())
            {
                return Err(TransactionResult::TemMalformed);
            }
            return Ok(());
        }

        // Setting a signer list -- entries are required
        let entries = ctx
            .tx
            .get("SignerEntries")
            .and_then(|v| v.as_array())
            .ok_or(TransactionResult::TemMalformed)?;

        if entries.is_empty() || entries.len() > 32 {
            return Err(TransactionResult::TemMalformed);
        }

        // Quorum must not exceed sum of weights
        let total_weight: u64 = entries
            .iter()
            .filter_map(|e| {
                e.get("SignerEntry")
                    .and_then(|se| se.get("SignerWeight"))
                    .and_then(|w| w.as_u64())
            })
            .sum();

        if (quorum as u64) > total_weight {
            return Err(TransactionResult::TemBadQuorum);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let key = keylet::account(&account_id);

        if !ctx.view.exists(&key) {
            return Err(TransactionResult::TerNoAccount);
        }
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let acct_key = keylet::account(&account_id);
        let sl_key = keylet::signer_list(&account_id);

        let quorum = ctx.tx["SignerQuorum"].as_u64().unwrap_or(0) as u32;
        let existing = ctx.view.exists(&sl_key);

        if quorum == 0 {
            // Delete signer list
            if !existing {
                return Err(TransactionResult::TecNoEntry);
            }
            remove_from_owner_dir(ctx.view, &account_id, &sl_key)?;
            ctx.view
                .erase(&sl_key)
                .map_err(|_| TransactionResult::TecNoEntry)?;

            // Decrement owner count
            let bytes = ctx
                .view
                .read(&acct_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut acct: Value =
                serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;
            helpers::adjust_owner_count(&mut acct, -1);
            let new_bytes =
                serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .update(acct_key, new_bytes)
                .map_err(|_| TransactionResult::TemMalformed)?;
        } else {
            // The signer list carries lsfOneOwnerCount and counts as a single
            // owner-reserve item (MultiSignReserve, retired/permanent).
            let flags = LSF_ONE_OWNER_COUNT;
            // rippled's writeSignersToSLE always sets sfSignerListID to the
            // default signer-list id (0); both sfSignerListID and sfOwnerNode are
            // SoeRequired and serialized onto the SLE.
            let mut sl_obj = serde_json::json!({
                "LedgerEntryType": "SignerList",
                "Owner": account_str,
                "SignerQuorum": quorum,
                "SignerEntries": ctx.tx.get("SignerEntries").cloned().unwrap_or(Value::Array(vec![])),
                "Flags": flags,
                // SoeRequired, default signer-list id.
                "SignerListID": 0u32,
                // Placeholder filled by the engine's central PreviousTxnID stamping.
                "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
                "PreviousTxnLgrSeq": 0,
            });

            if existing {
                // Preserve the owner-directory page hint of the list being
                // replaced (rippled removes and re-adds the entry, which lands on
                // the same page in the common single-page directory case).
                if let Some(bytes) = ctx.view.read(&sl_key) {
                    if let Ok(old) = serde_json::from_slice::<Value>(&bytes) {
                        if let Some(node) = old.get("OwnerNode").and_then(|v| v.as_str()) {
                            sl_obj["OwnerNode"] = Value::String(node.to_string());
                        }
                    }
                }
                let sl_bytes =
                    serde_json::to_vec(&sl_obj).map_err(|_| TransactionResult::TemMalformed)?;
                ctx.view
                    .update(sl_key, sl_bytes)
                    .map_err(|_| TransactionResult::TemMalformed)?;
            } else {
                // Link into the owner directory first so the page index can be
                // recorded as the SoeRequired sfOwnerNode.
                let owner_node = add_to_owner_dir(ctx.view, &account_id, &sl_key)?;
                sl_obj["OwnerNode"] = Value::String(format!("{owner_node:016X}"));
                let sl_bytes =
                    serde_json::to_vec(&sl_obj).map_err(|_| TransactionResult::TemMalformed)?;
                ctx.view
                    .insert(sl_key, sl_bytes)
                    .map_err(|_| TransactionResult::TemMalformed)?;
            }

            // Update account
            let bytes = ctx
                .view
                .read(&acct_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut acct: Value =
                serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;
            if !existing {
                helpers::adjust_owner_count(&mut acct, 1);
            }
            let new_bytes =
                serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .update(acct_key, new_bytes)
                .map_err(|_| TransactionResult::TemMalformed)?;
        }

        Ok(TransactionResult::TesSuccess)
    }
}
