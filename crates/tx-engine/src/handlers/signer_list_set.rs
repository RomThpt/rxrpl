use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::helpers;
use crate::owner_dir::{add_to_owner_dir, remove_from_owner_dir};
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

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
            helpers::increment_sequence(&mut acct);
            let new_bytes =
                serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .update(acct_key, new_bytes)
                .map_err(|_| TransactionResult::TemMalformed)?;
        } else {
            // Create or update signer list
            let sl_obj = serde_json::json!({
                "LedgerEntryType": "SignerList",
                "SignerQuorum": quorum,
                "SignerEntries": ctx.tx.get("SignerEntries").cloned().unwrap_or(Value::Array(vec![])),
                "SignerListID": 0,
                "Flags": 0,
            });

            let sl_bytes =
                serde_json::to_vec(&sl_obj).map_err(|_| TransactionResult::TemMalformed)?;

            if existing {
                ctx.view
                    .update(sl_key, sl_bytes)
                    .map_err(|_| TransactionResult::TemMalformed)?;
            } else {
                ctx.view
                    .insert(sl_key, sl_bytes)
                    .map_err(|_| TransactionResult::TemMalformed)?;
                add_to_owner_dir(ctx.view, &account_id, &sl_key)?;
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
            helpers::increment_sequence(&mut acct);
            let new_bytes =
                serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .update(acct_key, new_bytes)
                .map_err(|_| TransactionResult::TemMalformed)?;
        }

        Ok(TransactionResult::TesSuccess)
    }
}
