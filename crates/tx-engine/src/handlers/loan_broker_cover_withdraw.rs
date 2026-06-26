use rxrpl_amount::number::Number;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanBrokerCoverWithdrawTransactor;

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

impl Transactor for LoanBrokerCoverWithdrawTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        loan_broker_id(ctx.tx)?;
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
        let broker_key = loan_broker_id(ctx.tx)?;
        if ctx.view.read(&broker_key).is_none() {
            return Err(TransactionResult::TecNoEntry);
        }
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        // The cover returns to the destination (default: the caller).
        let dst_id = match helpers::get_str_field(ctx.tx, "Destination") {
            Some(d) => decode_account_id(d).map_err(|_| TransactionResult::TemInvalidAccountId)?,
            None => account_id,
        };

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
        let amount_num = Number::from_iou(&crate::amm_helpers::parse_iou_value(value));

        let broker_key = loan_broker_id(ctx.tx)?;
        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let pseudo_id = decode_account_id(
            broker["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let cover = Number::from_iou(&crate::amm_helpers::parse_iou_value(
            broker
                .get("CoverAvailable")
                .and_then(|v| v.as_str())
                .unwrap_or("0"),
        ));
        broker["CoverAvailable"] =
            serde_json::Value::String(cover.sub(&amount_num).to_decimal_string());
        ctx.view
            .update(
                broker_key,
                serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Move the cover from the broker pseudo's holding to the destination.
        let pseudo_hold =
            crate::amm_helpers::iou_holding_number(ctx.view, &pseudo_id, &issuer, &currency);
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &pseudo_id,
            &issuer,
            &currency,
            &pseudo_hold.sub(&amount_num),
        )?;
        let dst_hold =
            crate::amm_helpers::iou_holding_number(ctx.view, &dst_id, &issuer, &currency);
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &dst_id,
            &issuer,
            &currency,
            &dst_hold.add(&amount_num),
        )?;

        // Bump the caller's sequence.
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
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
