use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct ClawbackTransactor;

impl Transactor for ClawbackTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Amount must be an IOU object
        let amount = ctx
            .tx
            .get("Amount")
            .ok_or(TransactionResult::TemBadAmount)?;

        if !amount.is_object() {
            return Err(TransactionResult::TemBadAmount);
        }

        // Must have currency, issuer (holder), and value
        let currency = amount
            .get("currency")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemBadCurrency)?;
        if currency.is_empty() {
            return Err(TransactionResult::TemBadCurrency);
        }

        amount
            .get("issuer")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemBadIssuer)?;

        let value = amount
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemBadAmount)?;

        let parsed: f64 = value.parse().map_err(|_| TransactionResult::TemBadAmount)?;
        if parsed <= 0.0 {
            return Err(TransactionResult::TemBadAmount);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let issuer_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, issuer_str)?;

        // Amount.issuer is the holder
        let amount = ctx.tx.get("Amount").unwrap();
        let holder_str = amount["issuer"]
            .as_str()
            .ok_or(TransactionResult::TemBadIssuer)?;
        helpers::read_account_by_address(ctx.view, holder_str)?;

        // Verify trust line exists
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let holder_id =
            decode_account_id(holder_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let currency = amount["currency"]
            .as_str()
            .ok_or(TransactionResult::TemBadCurrency)?;
        let currency_bytes = helpers::currency_to_bytes(currency);

        let tl_key = keylet::trust_line(&issuer_id, &holder_id, &currency_bytes);
        let tl_bytes = ctx.view.read(&tl_key).ok_or(TransactionResult::TecNoEntry)?;
        let tl: Value =
            serde_json::from_slice(&tl_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Check holder has positive balance
        let balance_val = tl
            .get("Balance")
            .and_then(|b| b.get("value"))
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TecNoEntry)?;

        let balance: f64 = balance_val.parse().unwrap_or(0.0);

        // The balance in RippleState is from the perspective of the low account.
        // If issuer is the low account, a positive balance means the high account (holder) owes.
        // If issuer is the high account, a negative balance means the low account (holder) owes.
        let is_issuer_low = issuer_id.as_bytes() < holder_id.as_bytes();
        let holder_balance = if is_issuer_low { balance } else { -balance };

        if holder_balance <= 0.0 {
            return Err(TransactionResult::TecNoEntry);
        }

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let issuer_str = helpers::get_account(ctx.tx)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let amount = ctx.tx.get("Amount").unwrap();
        let holder_str = amount["issuer"]
            .as_str()
            .ok_or(TransactionResult::TemBadIssuer)?
            .to_string();
        let holder_id = decode_account_id(&holder_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let currency = amount["currency"]
            .as_str()
            .ok_or(TransactionResult::TemBadCurrency)?;
        let currency_bytes = helpers::currency_to_bytes(currency);
        let clawback_value: f64 = amount["value"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(TransactionResult::TemBadAmount)?;

        // Read trust line
        let tl_key = keylet::trust_line(&issuer_id, &holder_id, &currency_bytes);
        let tl_bytes = ctx
            .view
            .read(&tl_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut tl: Value =
            serde_json::from_slice(&tl_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Get current balance
        let balance_str = tl["Balance"]["value"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        let balance: f64 = balance_str.parse().unwrap_or(0.0);

        let is_issuer_low = issuer_id.as_bytes() < holder_id.as_bytes();
        let holder_balance = if is_issuer_low { balance } else { -balance };

        // Cap clawback to holder's actual balance
        let actual_clawback = clawback_value.min(holder_balance);

        // Update balance (reduce holder's balance)
        let new_balance = if is_issuer_low {
            balance - actual_clawback
        } else {
            balance + actual_clawback
        };

        tl["Balance"]["value"] = Value::String(format!("{new_balance}"));

        let tl_data =
            serde_json::to_vec(&tl).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(tl_key, tl_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Increment issuer sequence
        let issuer_acct_key = keylet::account(&issuer_id);
        let issuer_bytes = ctx
            .view
            .read(&issuer_acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut issuer_acct: Value =
            serde_json::from_slice(&issuer_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut issuer_acct);
        let issuer_data =
            serde_json::to_vec(&issuer_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(issuer_acct_key, issuer_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const ISSUER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const HOLDER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_trust_line(balance_value: &str) -> Ledger {
        let mut ledger = Ledger::genesis();

        // Setup both accounts
        for (addr, bal) in [(ISSUER, "100000000"), (HOLDER, "50000000")] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": bal,
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

        // Create trust line with holder having a balance
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let holder_id = decode_account_id(HOLDER).unwrap();
        let currency_bytes = helpers::currency_to_bytes("USD");
        let tl_key = keylet::trust_line(&issuer_id, &holder_id, &currency_bytes);

        let is_issuer_low = issuer_id.as_bytes() < holder_id.as_bytes();
        let (low_addr, high_addr) = if is_issuer_low {
            (ISSUER, HOLDER)
        } else {
            (HOLDER, ISSUER)
        };

        // Balance from the low account's perspective:
        // If issuer is low, positive means holder owes issuer (holder has tokens).
        // If holder is low, negative means holder owes issuer (holder has tokens).
        let stored_balance = if is_issuer_low {
            balance_value.to_string()
        } else {
            // Negate for storage
            let val: f64 = balance_value.parse().unwrap();
            format!("{}", -val)
        };

        let tl_obj = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {
                "currency": "USD",
                "issuer": ISSUER,
                "value": stored_balance
            },
            "LowLimit": {
                "currency": "USD",
                "issuer": low_addr,
                "value": "0"
            },
            "HighLimit": {
                "currency": "USD",
                "issuer": high_addr,
                "value": "1000"
            },
            "Flags": 0,
        });
        ledger
            .put_state(tl_key, serde_json::to_vec(&tl_obj).unwrap())
            .unwrap();

        ledger
    }

    #[test]
    fn clawback_partial() {
        let ledger = setup_with_trust_line("100");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "Clawback",
            "Account": ISSUER,
            "Amount": {
                "currency": "USD",
                "issuer": HOLDER,
                "value": "30"
            },
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = ClawbackTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify balance reduced
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let holder_id = decode_account_id(HOLDER).unwrap();
        let currency_bytes = helpers::currency_to_bytes("USD");
        let tl_key = keylet::trust_line(&issuer_id, &holder_id, &currency_bytes);
        let tl_bytes = sandbox.read(&tl_key).unwrap();
        let tl: Value = serde_json::from_slice(&tl_bytes).unwrap();

        let balance: f64 = tl["Balance"]["value"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();

        let is_issuer_low = issuer_id.as_bytes() < holder_id.as_bytes();
        let holder_balance = if is_issuer_low { balance } else { -balance };
        assert!((holder_balance - 70.0).abs() < 0.001);
    }

    #[test]
    fn clawback_total() {
        let ledger = setup_with_trust_line("50");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "Clawback",
            "Account": ISSUER,
            "Amount": {
                "currency": "USD",
                "issuer": HOLDER,
                "value": "100"
            },
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = ClawbackTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify balance is 0 (capped at actual balance)
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let holder_id = decode_account_id(HOLDER).unwrap();
        let currency_bytes = helpers::currency_to_bytes("USD");
        let tl_key = keylet::trust_line(&issuer_id, &holder_id, &currency_bytes);
        let tl_bytes = sandbox.read(&tl_key).unwrap();
        let tl: Value = serde_json::from_slice(&tl_bytes).unwrap();

        let balance: f64 = tl["Balance"]["value"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(balance.abs() < 0.001);
    }

    #[test]
    fn reject_no_trust_line() {
        let mut ledger = Ledger::genesis();
        // Setup accounts without trust line
        for (addr, bal) in [(ISSUER, "100000000"), (HOLDER, "50000000")] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": bal,
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "Clawback",
            "Account": ISSUER,
            "Amount": {
                "currency": "USD",
                "issuer": HOLDER,
                "value": "10"
            },
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            ClawbackTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn reject_xrp_amount() {
        let tx = serde_json::json!({
            "TransactionType": "Clawback",
            "Account": ISSUER,
            "Amount": "1000000",
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            ClawbackTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn reject_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "Clawback",
            "Account": ISSUER,
            "Amount": {
                "currency": "USD",
                "issuer": HOLDER,
                "value": "0"
            },
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            ClawbackTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }
}
