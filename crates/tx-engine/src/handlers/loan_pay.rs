use rxrpl_amount::number::Number;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanPayTransactor;

fn hash_field(tx: &serde_json::Value, field: &str) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, field).ok_or(TransactionResult::TemMalformed)?;
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

impl Transactor for LoanPayTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        hash_field(ctx.tx, "LoanID")?;
        let amount = ctx
            .tx
            .get("Amount")
            .ok_or(TransactionResult::TemBadAmount)?;
        let value = amount
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemBadAmount)?;
        if value
            .trim_start_matches('-')
            .trim_start_matches('0')
            .is_empty()
        {
            return Err(TransactionResult::TemBadAmount);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        let loan_key = hash_field(ctx.tx, "LoanID")?;
        if ctx.view.read(&loan_key).is_none() {
            return Err(TransactionResult::TecNoEntry);
        }
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let amount = ctx
            .tx
            .get("Amount")
            .ok_or(TransactionResult::TemBadAmount)?;
        let value = amount
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemBadAmount)?;
        let issuer = decode_account_id(
            amount
                .get("issuer")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TemBadIssuer)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let currency = helpers::currency_to_bytes(
            amount
                .get("currency")
                .and_then(|v| v.as_str())
                .unwrap_or_default(),
        );

        let loan_key = hash_field(ctx.tx, "LoanID")?;
        let loan_bytes = ctx
            .view
            .read(&loan_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut loan: serde_json::Value =
            serde_json::from_slice(&loan_bytes).map_err(|_| TransactionResult::TefInternal)?;

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
        let mut broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;
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
        let vault_pseudo_id = decode_account_id(
            vault["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Zero-interest loan: the whole payment is principal (no interest or
        // management fee). Only full / final payment is byte-verified.
        let amount_num = Number::from_iou(&crate::amm_helpers::parse_iou_value(value));
        let total_value = parse_num(&loan, "TotalValueOutstanding");
        if amount_num.sub(&total_value).negative() {
            // partial payment not yet handled
            return Err(TransactionResult::TemDisabled);
        }
        let principal_paid = total_value;

        // Mark the loan fully paid: outstanding -> 0 (omitted), record the last
        // due date, drop the remaining-payment schedule.
        let lo = loan.as_object_mut().unwrap();
        lo.remove("PrincipalOutstanding");
        lo.remove("TotalValueOutstanding");
        lo.remove("PaymentRemaining");
        if let Some(next) = lo.remove("NextPaymentDueDate") {
            lo.insert("PreviousPaymentDueDate".to_string(), next);
        }
        ctx.view
            .update(
                loan_key,
                serde_json::to_vec(&loan).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Broker: reduce the aggregate debt.
        let debt = parse_num(&broker, "DebtTotal");
        let new_debt = debt.sub(&principal_paid);
        if new_debt.is_zero() {
            broker.as_object_mut().unwrap().remove("DebtTotal");
        } else {
            broker["DebtTotal"] = serde_json::Value::String(new_debt.to_decimal_string());
        }
        ctx.view
            .update(
                broker_key,
                serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Vault: the repaid principal returns to the available pool.
        let avail = parse_num(&vault, "AssetsAvailable");
        vault["AssetsAvailable"] =
            serde_json::Value::String(avail.add(&principal_paid).to_decimal_string());
        ctx.view
            .update(
                vault_key,
                serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Move the payment from the borrower to the vault pseudo-account.
        let b_hold =
            crate::amm_helpers::iou_holding_number(ctx.view, &account_id, &issuer, &currency);
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &account_id,
            &issuer,
            &currency,
            &b_hold.sub(&principal_paid),
        )?;
        let vp_hold =
            crate::amm_helpers::iou_holding_number(ctx.view, &vault_pseudo_id, &issuer, &currency);
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &vault_pseudo_id,
            &issuer,
            &currency,
            &vp_hold.add(&principal_paid),
        )?;

        // Bump the payer's sequence.
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
