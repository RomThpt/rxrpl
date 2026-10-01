use rxrpl_amendment::feature::feature_id;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_ledger::sle_codec::decode_state;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::amount_helpers::{compute_holder_balance, compute_new_iou_balance};
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct ClawbackTransactor;

const LSFT_MPT_CAN_CLAWBACK: u32 = 0x0040;
const MAX_MPT_AMOUNT: u64 = 0x7FFF_FFFF_FFFF_FFFF;

struct MptClawback {
    issuance_key: Hash256,
    holder_id: rxrpl_primitives::AccountId,
    requested: u64,
}

fn parse_mpt_clawback(tx: &Value) -> Result<Option<MptClawback>, TransactionResult> {
    let Some(amount) = tx.get("Amount").and_then(Value::as_object) else {
        return Ok(None);
    };
    let Some(issuance_id) = amount.get("mpt_issuance_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    let raw = hex::decode(issuance_id).map_err(|_| TransactionResult::TemMalformed)?;
    if raw.len() != 24 {
        return Err(TransactionResult::TemMalformed);
    }
    let requested = amount
        .get("value")
        .and_then(Value::as_str)
        .ok_or(TransactionResult::TemBadAmount)?
        .parse::<u64>()
        .map_err(|_| TransactionResult::TemBadAmount)?;
    let sequence = u32::from_be_bytes(raw[..4].try_into().unwrap());
    let issuer = rxrpl_primitives::AccountId::from_slice(&raw[4..])
        .map_err(|_| TransactionResult::TemMalformed)?;
    let holder = helpers::get_str_field(tx, "Holder").ok_or(TransactionResult::TemMalformed)?;
    let holder_id =
        decode_account_id(holder).map_err(|_| TransactionResult::TemInvalidAccountId)?;

    Ok(Some(MptClawback {
        issuance_key: keylet::mptoken_issuance(&issuer, sequence),
        holder_id,
        requested,
    }))
}

fn uint64_field(entry: &Value, field: &str) -> Result<u64, TransactionResult> {
    entry
        .get(field)
        .and_then(Value::as_str)
        .ok_or(TransactionResult::TefInternal)?
        .parse::<u64>()
        .map_err(|_| TransactionResult::TefInternal)
}

fn mpt_amount(entry: &Value) -> Result<u64, TransactionResult> {
    uint64_field(entry, "MPTAmount")
}

impl Transactor for ClawbackTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if let Some(mpt) = parse_mpt_clawback(ctx.tx)? {
            if !ctx.rules.enabled(&feature_id("MPTokensV1")) {
                return Err(TransactionResult::TemDisabled);
            }
            let account = decode_account_id(helpers::get_account(ctx.tx)?)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let holder =
                helpers::get_str_field(ctx.tx, "Holder").ok_or(TransactionResult::TemMalformed)?;
            let holder =
                decode_account_id(holder).map_err(|_| TransactionResult::TemInvalidAccountId)?;
            if account == holder {
                return Err(TransactionResult::TemMalformed);
            }
            if mpt.requested == 0 || mpt.requested > MAX_MPT_AMOUNT {
                return Err(TransactionResult::TemBadAmount);
            }
            return Ok(());
        }

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
        let (_, issuer_acct) = helpers::read_account_by_address(ctx.view, issuer_str)?;

        if let Some(mpt) = parse_mpt_clawback(ctx.tx)? {
            let holder_str =
                helpers::get_str_field(ctx.tx, "Holder").ok_or(TransactionResult::TemMalformed)?;
            helpers::read_account_by_address(ctx.view, holder_str)?;

            let issuance_bytes = ctx
                .view
                .read(&mpt.issuance_key)
                .ok_or(TransactionResult::TecNoEntry)?;
            let issuance =
                decode_state(&issuance_bytes).map_err(|_| TransactionResult::TefInternal)?;
            if helpers::get_flags(&issuance) & LSFT_MPT_CAN_CLAWBACK == 0
                || issuance.get("Issuer").and_then(Value::as_str) != Some(issuer_str)
            {
                return Err(TransactionResult::TecNoPermission);
            }

            let mptoken_key = keylet::mptoken(mpt.issuance_key.as_bytes(), &mpt.holder_id);
            let mptoken_bytes = ctx
                .view
                .read(&mptoken_key)
                .ok_or(TransactionResult::TecNoEntry)?;
            if mpt_amount(
                &decode_state(&mptoken_bytes).map_err(|_| TransactionResult::TefInternal)?,
            )? == 0
            {
                return Err(TransactionResult::TecInsufficientFunds);
            }
            return Ok(());
        }

        // Issuer must have lsfAllowTrustLineClawback set on its AccountRoot.
        const LSF_ALLOW_TRUST_LINE_CLAWBACK: u32 = 0x8000_0000;
        let issuer_flags = helpers::get_flags(&issuer_acct);
        if issuer_flags & LSF_ALLOW_TRUST_LINE_CLAWBACK == 0 {
            return Err(TransactionResult::TecNoPermission);
        }

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
        let tl_bytes = ctx
            .view
            .read(&tl_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let tl: Value = helpers::decode_state_value(&tl_bytes)?;

        let holder_balance = compute_holder_balance(&tl, &issuer_id, &holder_id);
        if holder_balance <= 0.0 {
            return Err(TransactionResult::TecNoEntry);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let issuer_str = helpers::get_account(ctx.tx)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        if let Some(mpt) = parse_mpt_clawback(ctx.tx)? {
            let issuance_bytes = ctx
                .view
                .read(&mpt.issuance_key)
                .ok_or(TransactionResult::TecNoEntry)?;
            let mut issuance =
                decode_state(&issuance_bytes).map_err(|_| TransactionResult::TefInternal)?;
            let mptoken_key = keylet::mptoken(mpt.issuance_key.as_bytes(), &mpt.holder_id);
            let mptoken_bytes = ctx
                .view
                .read(&mptoken_key)
                .ok_or(TransactionResult::TecNoEntry)?;
            let mut mptoken =
                decode_state(&mptoken_bytes).map_err(|_| TransactionResult::TefInternal)?;
            let held = mpt_amount(&mptoken)?;
            let actual = mpt.requested.min(held);
            if actual == 0 {
                return Err(TransactionResult::TecInsufficientFunds);
            }

            let remaining = held - actual;
            if remaining == 0 {
                mptoken
                    .as_object_mut()
                    .ok_or(TransactionResult::TefInternal)?
                    .remove("MPTAmount");
            } else {
                mptoken["MPTAmount"] = Value::String(remaining.to_string());
            }
            ctx.view
                .update(
                    mptoken_key,
                    serde_json::to_vec(&mptoken).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;

            let outstanding = uint64_field(&issuance, "OutstandingAmount")?;
            issuance["OutstandingAmount"] = Value::String(
                outstanding
                    .checked_sub(actual)
                    .ok_or(TransactionResult::TefInternal)?
                    .to_string(),
            );
            ctx.view
                .update(
                    mpt.issuance_key,
                    serde_json::to_vec(&issuance).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
            return Ok(TransactionResult::TesSuccess);
        }

        let amount = ctx.tx.get("Amount").unwrap();
        let holder_str = amount["issuer"]
            .as_str()
            .ok_or(TransactionResult::TemBadIssuer)?
            .to_string();
        let holder_id =
            decode_account_id(&holder_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

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
        let mut tl: Value = helpers::decode_state_value(&tl_bytes)?;

        let holder_balance = compute_holder_balance(&tl, &issuer_id, &holder_id);
        if holder_balance <= 0.0 {
            return Err(TransactionResult::TecNoEntry);
        }

        // Cap clawback to holder's actual balance.
        let actual_clawback = clawback_value.min(holder_balance);

        let new_balance =
            compute_new_iou_balance(&tl, &format!("-{actual_clawback}"), &issuer_id, &holder_id)?;
        tl["Balance"]["value"] = Value::String(new_balance);

        let tl_data = serde_json::to_vec(&tl).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(tl_key, tl_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // The issuer's Sequence/Ticket (and fee) are consumed centrally by the
        // engine (parent sandbox) before doApply.

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

        // RippleState Balance is stored from the low-account perspective;
        // a holder that is the low account holds a positive balance.
        let stored_balance = if is_issuer_low {
            let val: f64 = balance_value.parse().unwrap();
            format!("{}", -val)
        } else {
            balance_value.to_string()
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

        let balance: f64 = tl["Balance"]["value"].as_str().unwrap().parse().unwrap();

        let is_issuer_low = issuer_id.as_bytes() < holder_id.as_bytes();
        let holder_balance = if is_issuer_low { -balance } else { balance };
        assert!((holder_balance - 70.0).abs() < 0.001);
    }

    #[test]
    fn clawback_with_ticket_consumes_ticket_not_sequence() {
        let mut ledger = setup_with_trust_line("100");

        // Give the issuer a Ticket SLE at sequence 5 and an OwnerCount of 1.
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let ticket_seq = 5u32;
        let ticket_key = keylet::ticket(&issuer_id, ticket_seq);
        let ticket_obj = serde_json::json!({
            "LedgerEntryType": "Ticket",
            "Account": ISSUER,
            "TicketSequence": ticket_seq,
            "Flags": 0,
        });
        ledger
            .put_state(ticket_key, serde_json::to_vec(&ticket_obj).unwrap())
            .unwrap();

        let acct_key = keylet::account(&issuer_id);
        let mut acct: Value = serde_json::from_slice(ledger.get_state(&acct_key).unwrap()).unwrap();
        acct["OwnerCount"] = Value::from(1u64);
        ledger
            .put_state(acct_key, serde_json::to_vec(&acct).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "Clawback",
            "Account": ISSUER,
            "Amount": { "currency": "USD", "issuer": HOLDER, "value": "30" },
            "Fee": "12",
            "Sequence": 0,
            "TicketSequence": ticket_seq,
        });

        // Engine consumes the sender's Sequence/Ticket centrally before doApply.
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = ClawbackTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Ticket SLE consumed.
        assert!(!sandbox.exists(&keylet::ticket(&issuer_id, ticket_seq)));

        // Issuer Sequence untouched, OwnerCount decremented.
        let acct_bytes = sandbox.read(&keylet::account(&issuer_id)).unwrap();
        let acct: Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["Sequence"].as_u64(), Some(1));
        assert_eq!(acct["OwnerCount"].as_u64(), Some(0));
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

        let balance: f64 = tl["Balance"]["value"].as_str().unwrap().parse().unwrap();
        assert!(balance.abs() < 0.001);
    }

    fn setup_flagged_trust_line(balance_value: &str) -> Ledger {
        const LSF_ALLOW_TRUST_LINE_CLAWBACK: u32 = 0x8000_0000;
        let mut ledger = setup_with_trust_line(balance_value);
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let key = keylet::account(&issuer_id);
        let mut acct: Value =
            rxrpl_ledger::sle_codec::decode_state(ledger.get_state(&key).unwrap()).unwrap();
        acct["Flags"] = Value::from(LSF_ALLOW_TRUST_LINE_CLAWBACK);
        let json = serde_json::to_vec(&acct).unwrap();
        let binary = rxrpl_ledger::sle_codec::encode_sle(&json).unwrap();
        ledger.put_state(key, binary).unwrap();
        ledger
    }

    #[test]
    fn preclaim_accepts_positive_holder_balance() {
        let ledger = setup_flagged_trust_line("100");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "Clawback",
            "Account": ISSUER,
            "Amount": { "currency": "USD", "issuer": HOLDER, "value": "50" },
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(ClawbackTransactor.preclaim(&ctx), Ok(()));
    }

    #[test]
    fn reject_no_trust_line() {
        const LSF_ALLOW_TRUST_LINE_CLAWBACK: u32 = 0x8000_0000;
        let mut ledger = Ledger::genesis();
        // Setup accounts without trust line. Issuer must have the clawback
        // flag set so the preclaim's flag check passes and we exercise the
        // missing-trust-line branch.
        for (addr, bal, flags) in [
            (ISSUER, "100000000", LSF_ALLOW_TRUST_LINE_CLAWBACK),
            (HOLDER, "50000000", 0),
        ] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": bal,
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": flags,
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
