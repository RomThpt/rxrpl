use rxrpl_codec::address::classic::{account_id_from_public_key, decode_account_id};
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// `lsfPasswordSpent` (0x00010000): armed on an AccountRoot when its owner
/// spends the one free (zero-fee) regular-key reset. rippled clears it again on
/// the next fee-paying transaction that credits the account (see Payment).
const LSF_PASSWORD_SPENT: u64 = 0x0001_0000;

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

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let key = keylet::account(&account_id);

        let bytes = ctx.view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
        let mut obj: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;

        // rippled arms lsfPasswordSpent when the required minimum fee is zero,
        // regardless of the Fee amount the sender chose to pay. The one-time
        // zero base fee requires a valid master SigningPubKey and an unspent
        // password flag (SetRegularKey::calculateBaseFee / doApply).
        let flags = obj.get("Flags").and_then(Value::as_u64).unwrap_or(0);
        let master_signed = ctx
            .tx
            .get("SigningPubKey")
            .and_then(Value::as_str)
            .and_then(|key| hex::decode(key).ok())
            .filter(|key| rxrpl_crypto::KeyType::from_public_key(key).is_some())
            .is_some_and(|key| account_id_from_public_key(&key) == account_id);
        if ctx.fees.base_fee == 0 || (master_signed && flags & LSF_PASSWORD_SPENT == 0) {
            obj["Flags"] = Value::from(flags | LSF_PASSWORD_SPENT);
        }

        if let Some(reg_key) = ctx.tx.get("RegularKey") {
            obj["RegularKey"] = reg_key.clone();
        } else {
            obj.as_object_mut().unwrap().remove("RegularKey");
        }

        let new_bytes = serde_json::to_vec(&obj).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .update(key, new_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::ApplyContext;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_codec::address::classic::encode_account_id;
    use rxrpl_crypto::{KeyPair, KeyType, Seed};
    use rxrpl_ledger::Ledger;

    fn ledger_with_account(addr: &str, flags: u64, regular_key: &str) -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(addr).unwrap();
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": addr,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": flags,
            "RegularKey": regular_key,
        });
        ledger
            .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    fn apply_set_regular_key(fee: &str, master_signed: bool, flags: u64) -> serde_json::Value {
        let master = KeyPair::from_seed(&Seed::from_bytes([1u8; 16]), KeyType::Ed25519);
        let regular = KeyPair::from_seed(&Seed::from_bytes([2u8; 16]), KeyType::Ed25519);
        let account = encode_account_id(&account_id_from_public_key(&master.public_key.0));
        let regular_key = encode_account_id(&account_id_from_public_key(&regular.public_key.0));
        let signing_key = if master_signed {
            &master.public_key.0
        } else {
            &regular.public_key.0
        };
        let ledger = ledger_with_account(&account, flags, &regular_key);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "SetRegularKey",
            "Account": account,
            "RegularKey": regular_key,
            "Fee": fee,
            "Sequence": 1,
            "SigningPubKey": hex::encode_upper(signing_key),
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            SetRegularKeyTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );
        let id = decode_account_id(&account).unwrap();
        let bytes = sandbox.read(&keylet::account(&id)).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// A master-signed reset with an unspent password flag has zero required
    /// minimum fee, even if the sender offers more than that minimum.
    #[test]
    fn master_signed_reset_sets_password_spent_regardless_of_paid_fee() {
        for fee in ["0", "12"] {
            let acct = apply_set_regular_key(fee, true, 0);
            assert_eq!(acct["Flags"].as_u64().unwrap(), LSF_PASSWORD_SPENT);
        }
    }

    /// A regular-key signature does not qualify for the free minimum fee.
    #[test]
    fn regular_signed_reset_does_not_spend_password_allowance() {
        for fee in ["0", "12"] {
            let acct = apply_set_regular_key(fee, false, 0);
            assert_eq!(acct["Flags"].as_u64().unwrap(), 0);
        }
    }

    #[test]
    fn already_spent_password_flag_is_preserved() {
        let acct = apply_set_regular_key("12", true, LSF_PASSWORD_SPENT);
        assert_eq!(acct["Flags"].as_u64().unwrap(), LSF_PASSWORD_SPENT);
    }
}
