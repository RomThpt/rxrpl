use rxrpl_amount::number::Number;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanManageTransactor;

const TF_LOAN_DEFAULT: u32 = 0x0001_0000;
const TF_LOAN_IMPAIR: u32 = 0x0002_0000;
const TF_LOAN_UNIMPAIR: u32 = 0x0004_0000;
const LSF_LOAN_IMPAIRED: u32 = 0x0002_0000;

fn loan_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, "LoanID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    Hash256::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
}

fn parse_num(v: &serde_json::Value, f: &str) -> Number {
    Number::from_iou(&crate::amm_helpers::parse_iou_value(
        v.get(f).and_then(|x| x.as_str()).unwrap_or("0"),
    ))
}

impl Transactor for LoanManageTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        loan_id(ctx.tx)?;
        // Exactly one management action; only impair is byte-verified so far.
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let action = flags & (TF_LOAN_DEFAULT | TF_LOAN_IMPAIR | TF_LOAN_UNIMPAIR);
        if action.count_ones() > 1 {
            return Err(TransactionResult::TemInvalidFlag);
        }
        if action != 0 && action != TF_LOAN_IMPAIR {
            return Err(TransactionResult::TemDisabled);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        let loan_key = loan_id(ctx.tx)?;
        if ctx.view.read(&loan_key).is_none() {
            return Err(TransactionResult::TecNoEntry);
        }
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);

        let loan_key = loan_id(ctx.tx)?;
        let loan_bytes = ctx
            .view
            .read(&loan_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut loan: serde_json::Value =
            serde_json::from_slice(&loan_bytes).map_err(|_| TransactionResult::TefInternal)?;

        if flags & TF_LOAN_IMPAIR != 0 {
            // Mark the loan impaired and book its outstanding value as an
            // unrealized loss on the vault.
            let owed = parse_num(&loan, "TotalValueOutstanding");

            let loan_flags = loan.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            loan["Flags"] = serde_json::Value::from(loan_flags | LSF_LOAN_IMPAIRED);
            ctx.view
                .update(
                    loan_key,
                    serde_json::to_vec(&loan).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;

            let broker_key = {
                let id = loan["LoanBrokerID"]
                    .as_str()
                    .ok_or(TransactionResult::TefInternal)?;
                Hash256::from_slice(&hex::decode(id).map_err(|_| TransactionResult::TefInternal)?)
                    .map_err(|_| TransactionResult::TefInternal)?
            };
            let broker_bytes = ctx
                .view
                .read(&broker_key)
                .ok_or(TransactionResult::TecNoEntry)?;
            let broker: serde_json::Value = serde_json::from_slice(&broker_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
            let vault_key = {
                let id = broker["VaultID"]
                    .as_str()
                    .ok_or(TransactionResult::TefInternal)?;
                Hash256::from_slice(&hex::decode(id).map_err(|_| TransactionResult::TefInternal)?)
                    .map_err(|_| TransactionResult::TefInternal)?
            };
            let vault_bytes = ctx
                .view
                .read(&vault_key)
                .ok_or(TransactionResult::TecNoEntry)?;
            let mut vault: serde_json::Value =
                serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;
            let loss = parse_num(&vault, "LossUnrealized");
            vault["LossUnrealized"] =
                serde_json::Value::String(loss.add(&owed).to_decimal_string());
            ctx.view
                .update(
                    vault_key,
                    serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Bump the caller's sequence.
        let acct_key = keylet::account(&account_id);
        let ab = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&ab).map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut account);
        ctx.view
            .update(
                acct_key,
                serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}
