use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::pseudo;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanBrokerSetTransactor;

const ZERO_TXID: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const LSF_DISABLE_MASTER: u32 = 0x0010_0000;
const LSF_DEFAULT_RIPPLE: u32 = 0x0080_0000;
const LSF_DEPOSIT_AUTH: u32 = 0x0100_0000;

const MAX_MANAGEMENT_FEE_RATE: u32 = 10_000;
const MAX_COVER_RATE: u32 = 100_000;

fn vault_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, "VaultID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    Hash256::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
}

impl Transactor for LoanBrokerSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Modifying an existing broker (by LoanBrokerID) is not byte-verified yet.
        if helpers::get_str_field(ctx.tx, "LoanBrokerID").is_some() {
            return Err(TransactionResult::TemDisabled);
        }
        vault_id(ctx.tx)?;

        if let Some(rate) = helpers::get_u32_field(ctx.tx, "ManagementFeeRate") {
            if rate > MAX_MANAGEMENT_FEE_RATE {
                return Err(TransactionResult::TemInvalid);
            }
        }
        let cover_min = helpers::get_u32_field(ctx.tx, "CoverRateMinimum").unwrap_or(0);
        let cover_liq = helpers::get_u32_field(ctx.tx, "CoverRateLiquidation").unwrap_or(0);
        if cover_min > MAX_COVER_RATE || cover_liq > MAX_COVER_RATE {
            return Err(TransactionResult::TemInvalid);
        }
        // Both cover rates must be zero or both non-zero.
        if (cover_min == 0) != (cover_liq == 0) {
            return Err(TransactionResult::TemInvalid);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        let vault_key = vault_id(ctx.tx)?;
        if ctx.view.read(&vault_key).is_none() {
            return Err(TransactionResult::TecNoEntry);
        }
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        // The loan-broker keylet/Sequence is the TX seq-proxy value (the engine
        // already consumed the sender's Sequence/Ticket centrally).
        let seq = helpers::tx_seq_proxy_value(ctx.tx);

        // Read the vault to learn its pseudo-account and asset.
        let vault_key = vault_id(ctx.tx)?;
        let vault_bytes = ctx
            .view
            .read(&vault_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let vault_pseudo_id = decode_account_id(
            vault["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let vault_asset = vault.get("Asset").cloned();
        let is_iou = vault_asset
            .as_ref()
            .map(|a| !pseudo::is_xrp_asset(a))
            .unwrap_or(false);

        // 1. Broker keylet + its pseudo-account.
        let broker_key = keylet::loan_broker(account_id.as_bytes(), seq);
        let pseudo_id = pseudo::derive_pseudo_account(ctx, &broker_key)?;
        let pseudo_str = encode_account_id(&pseudo_id);
        let pseudo_owner_count = if is_iou { 1 } else { 0 };

        let pseudo_acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": pseudo_str,
            "Balance": "0",
            "Flags": LSF_DISABLE_MASTER | LSF_DEFAULT_RIPPLE | LSF_DEPOSIT_AUTH,
            "OwnerCount": pseudo_owner_count,
            // Pseudo-accounts are created with Sequence 0 (SoeRequired on
            // AccountRoot, serialized even at its default).
            "Sequence": 0,
            "LoanBrokerID": hex::encode_upper(broker_key.as_bytes()),
            "PreviousTxnID": ZERO_TXID,
            "PreviousTxnLgrSeq": 0,
        });
        ctx.view
            .insert(
                keylet::account(&pseudo_id),
                serde_json::to_vec(&pseudo_acct).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // 2. The LoanBroker object, linked into the owner's and the vault
        //    pseudo-account's directories.
        let mut broker = serde_json::json!({
            "LedgerEntryType": "LoanBroker",
            "Account": pseudo_str,
            "Owner": account_str,
            "Sequence": seq,
            "VaultID": hex::encode_upper(vault_key.as_bytes()),
            "LoanSequence": 1,
            // Default sfFlags is serialized (SoeRequired common field).
            "Flags": 0u32,
            "PreviousTxnID": ZERO_TXID,
            "PreviousTxnLgrSeq": 0,
        });
        if let Some(rate) = helpers::get_u32_field(ctx.tx, "ManagementFeeRate") {
            broker["ManagementFeeRate"] = serde_json::Value::from(rate);
        }
        if let Some(rate) = helpers::get_u32_field(ctx.tx, "CoverRateMinimum") {
            broker["CoverRateMinimum"] = serde_json::Value::from(rate);
        }
        if let Some(rate) = helpers::get_u32_field(ctx.tx, "CoverRateLiquidation") {
            broker["CoverRateLiquidation"] = serde_json::Value::from(rate);
        }
        if let Some(debt) = helpers::get_u64_str_field(ctx.tx, "DebtMaximum") {
            broker["DebtMaximum"] = serde_json::Value::String(debt.to_string());
        }
        if let Some(data) = helpers::get_str_field(ctx.tx, "Data") {
            broker["Data"] = serde_json::Value::String(data.to_string());
        }
        // rippled links the broker into the owner's directory as sfOwnerNode and
        // into the vault pseudo-account's directory as sfVaultNode; both are
        // SoeRequired and written unconditionally (even when the page hint is 0).
        let owner_node = crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &broker_key)?;
        broker["OwnerNode"] = serde_json::Value::String(format!("{owner_node:016X}"));
        let vault_node =
            crate::owner_dir::add_to_owner_dir(ctx.view, &vault_pseudo_id, &broker_key)?;
        broker["VaultNode"] = serde_json::Value::String(format!("{vault_node:016X}"));
        ctx.view
            .insert(
                broker_key,
                serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // 3. For an IOU vault, give the broker pseudo-account its empty holding.
        if is_iou {
            if let Some(asset) = &vault_asset {
                pseudo::create_empty_iou_line(ctx, &pseudo_id, asset)?;
            }
        }

        // 4. Owner: +2 owner count (broker + pseudo) and sequence bump.
        helpers::adjust_owner_count(&mut account, 2);
        ctx.view
            .update(
                acct_key,
                serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}
