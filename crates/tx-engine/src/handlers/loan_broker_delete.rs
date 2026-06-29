use rxrpl_amount::number::Number;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanBrokerDeleteTransactor;

/// Parse the 32-byte `LoanBrokerID` (the loan-broker keylet itself).
fn loan_broker_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str =
        helpers::get_str_field(tx, "LoanBrokerID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    Hash256::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
}

impl Transactor for LoanBrokerDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        loan_broker_id(ctx.tx)?;
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        let broker_key = loan_broker_id(ctx.tx)?;
        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;
        if broker["Owner"].as_str() != Some(account_str) {
            return Err(TransactionResult::TecNoPermission);
        }
        // Outstanding debt blocks deletion.
        let debt: u128 = broker
            .get("DebtTotal")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if debt != 0 {
            return Err(TransactionResult::TecHasObligations);
        }
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let broker_key = loan_broker_id(ctx.tx)?;
        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let pseudo_id = decode_account_id(
            broker["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Resolve the vault to learn its pseudo-account (for the VaultNode
        // directory) and the asset.
        let vault_key = {
            let vid = broker["VaultID"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?;
            Hash256::from_slice(&hex::decode(vid).map_err(|_| TransactionResult::TefInternal)?)
                .map_err(|_| TransactionResult::TefInternal)?
        };
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
        let asset = vault["Asset"].clone();
        let (cur_bytes, issuer_id) = crate::pseudo::asset_currency_issuer(&asset)?;

        // Return any remaining cover from the broker pseudo to the owner.
        let cover = Number::from_iou(&crate::amm_helpers::parse_iou_value(
            broker
                .get("CoverAvailable")
                .and_then(|v| v.as_str())
                .unwrap_or("0"),
        ));
        if !cover.is_zero() {
            let pseudo_hold = crate::amm_helpers::iou_holding_number(
                ctx.view, &pseudo_id, &issuer_id, &cur_bytes,
            );
            crate::amm_helpers::set_iou_holding(
                ctx.view,
                &pseudo_id,
                &issuer_id,
                &cur_bytes,
                &pseudo_hold.sub(&cover),
            )?;
            let owner_hold = crate::amm_helpers::iou_holding_number(
                ctx.view,
                &account_id,
                &issuer_id,
                &cur_bytes,
            );
            crate::amm_helpers::set_iou_holding(
                ctx.view,
                &account_id,
                &issuer_id,
                &cur_bytes,
                &owner_hold.add(&cover),
            )?;
        }

        // Remove the broker pseudo's now-empty trust line (from both the pseudo
        // and issuer directories) and restamp the issuer.
        let tl_key = keylet::trust_line(&pseudo_id, &issuer_id, &cur_bytes);
        ctx.view
            .erase(&tl_key)
            .map_err(|_| TransactionResult::TefInternal)?;
        crate::owner_dir::remove_from_owner_dir(ctx.view, &pseudo_id, &tl_key)?;
        crate::owner_dir::remove_from_owner_dir(ctx.view, &issuer_id, &tl_key)?;
        let issuer_key = keylet::account(&issuer_id);
        if let Some(bytes) = ctx.view.read(&issuer_key) {
            ctx.view
                .update(issuer_key, bytes.to_vec())
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Unlink the broker from the owner and vault-pseudo directories.
        crate::owner_dir::remove_from_owner_dir(ctx.view, &account_id, &broker_key)?;
        crate::owner_dir::remove_from_owner_dir(ctx.view, &vault_pseudo_id, &broker_key)?;

        // Erase the broker pseudo-account (owner count must reach 0 first, the
        // trust line having been removed).
        let pseudo_key = keylet::account(&pseudo_id);
        let pseudo_bytes = ctx
            .view
            .read(&pseudo_key)
            .ok_or(TransactionResult::TefBadLedger)?;
        let mut pseudo: serde_json::Value =
            serde_json::from_slice(&pseudo_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut pseudo, -1);
        ctx.view
            .update(
                pseudo_key,
                serde_json::to_vec(&pseudo).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .erase(&pseudo_key)
            .map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .erase(&broker_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Owner: -2 owner count (broker + pseudo) and sequence bump.
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut account, -2);
        ctx.view
            .update(
                acct_key,
                serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}
