use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::keylet;
use rxrpl_protocol::TransactionResult;
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// SetRegularKey transaction handler.
///
/// Sets or clears the regular key pair for an account.
/// If RegularKey is present, sets it; if absent, clears it.
pub struct SetRegularKeyTransactor;

impl Transactor for SetRegularKeyTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // If RegularKey is provided, validate it's a valid address
        if let Some(key) = ctx.tx.get("RegularKey").and_then(|v| v.as_str()) {
            decode_account_id(key).map_err(|_| TransactionResult::TemBadRegKey)?;
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

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let key = keylet::account(&account_id);

        let bytes = ctx
            .view
            .read(&key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut obj: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;

        if let Some(reg_key) = ctx.tx.get("RegularKey") {
            obj["RegularKey"] = reg_key.clone();
        } else {
            obj.as_object_mut().unwrap().remove("RegularKey");
        }

        helpers::increment_sequence(&mut obj);

        let new_bytes =
            serde_json::to_vec(&obj).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .update(key, new_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        Ok(TransactionResult::TesSuccess)
    }
}
