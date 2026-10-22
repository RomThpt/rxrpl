use rxrpl_amount::number::Number;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanSetTransactor;

const DEFAULT_PAYMENT_INTERVAL: u32 = 60;
const DEFAULT_PAYMENT_TOTAL: u32 = 1;
const DEFAULT_GRACE_PERIOD: u32 = 60;

fn loan_broker_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str =
        helpers::get_str_field(tx, "LoanBrokerID").ok_or(TransactionResult::TemMalformed)?;
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

impl Transactor for LoanSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        loan_broker_id(ctx.tx)?;
        helpers::get_str_field(ctx.tx, "PrincipalRequested")
            .ok_or(TransactionResult::TemMalformed)?;
        // Only zero-interest loans are byte-verified so far.
        if helpers::get_u32_field(ctx.tx, "InterestRate").unwrap_or(0) != 0 {
            return Err(TransactionResult::TemDisabled);
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

        let broker_key = loan_broker_id(ctx.tx)?;
        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let broker_owner = broker["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let broker_pseudo_id = decode_account_id(
            broker["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // The borrower is the counterparty (or the caller, when the caller is
        // not the broker owner).
        let counterparty = helpers::get_str_field(ctx.tx, "Counterparty")
            .map(|s| s.to_string())
            .unwrap_or_else(|| broker_owner.clone());
        let borrower_str = if counterparty == broker_owner {
            account_str.to_string()
        } else {
            counterparty
        };
        let borrower_id =
            decode_account_id(&borrower_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Resolve the vault for its pseudo-account, asset and available balance.
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
        let mut vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let vault_pseudo_id = decode_account_id(
            vault["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let asset = vault["Asset"].clone();
        let (cur_bytes, issuer_id) = crate::pseudo::asset_currency_issuer(&asset)?;

        let principal_str = helpers::get_str_field(ctx.tx, "PrincipalRequested")
            .ok_or(TransactionResult::TemMalformed)?;
        let principal = Number::from_iou(&crate::amm_helpers::parse_iou_value(principal_str));

        let assets_available = parse_num(&vault, "AssetsAvailable");
        if assets_available.sub(&principal).negative() {
            return Err(TransactionResult::TecInsufficientFunds);
        }

        // Zero-interest schedule: value == principal, even payments, no fee.
        let payment_total = helpers::get_u32_field(ctx.tx, "PaymentTotal")
            .unwrap_or(DEFAULT_PAYMENT_TOTAL)
            .max(1);
        let payment_interval =
            helpers::get_u32_field(ctx.tx, "PaymentInterval").unwrap_or(DEFAULT_PAYMENT_INTERVAL);
        let grace_period =
            helpers::get_u32_field(ctx.tx, "GracePeriod").unwrap_or(DEFAULT_GRACE_PERIOD);
        let periodic_payment = principal.div(&Number::from_int(payment_total as i64));

        let loan_sequence = broker["LoanSequence"].as_u64().unwrap_or(1) as u32;
        // The loan scale is the vault asset's representation scale (the exponent
        // of the assets-total IOU value).
        let loan_scale = rxrpl_amount::iou::IOUAmount::from_decimal_string(
            vault
                .get("AssetsTotal")
                .and_then(|v| v.as_str())
                .unwrap_or("0"),
        )
        .map(|a| a.exponent())
        .unwrap_or(0);

        let start_date = ctx.view.close_time();
        let loan_key = keylet::loan(broker_key.as_bytes(), loan_sequence);

        // 1. Create the Loan object, linked into the borrower and broker-pseudo
        //    directories.
        let mut loan = serde_json::json!({
            "LedgerEntryType": "Loan",
            "Borrower": borrower_str,
            "GracePeriod": grace_period,
            "LoanBrokerID": hex::encode_upper(broker_key.as_bytes()),
            "LoanScale": loan_scale,
            "LoanSequence": loan_sequence,
            "NextPaymentDueDate": start_date + payment_interval,
            "PaymentInterval": payment_interval,
            "PaymentRemaining": payment_total,
            "PeriodicPayment": periodic_payment.to_decimal_string(),
            "PrincipalOutstanding": principal.to_decimal_string(),
            "StartDate": start_date,
            "TotalValueOutstanding": principal.to_decimal_string(),
            // Default sfFlags is serialized (SoeRequired common field).
            "Flags": 0u32,
            "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
            "PreviousTxnLgrSeq": 0,
        });
        // rippled links the loan into the borrower's directory as sfOwnerNode and
        // into the broker pseudo-account's directory as sfLoanBrokerNode; both are
        // SoeRequired and written unconditionally (even when the page hint is 0).
        // (The old code wrote a bogus "BorrowerNode" field — dropped by the codec
        // as unknown — and omitted both pointers when the page was 0.)
        let owner_node = crate::owner_dir::add_to_owner_dir(ctx.view, &borrower_id, &loan_key)?;
        loan["OwnerNode"] = serde_json::Value::String(format!("{owner_node:016X}"));
        let broker_node =
            crate::owner_dir::add_to_owner_dir(ctx.view, &broker_pseudo_id, &loan_key)?;
        loan["LoanBrokerNode"] = serde_json::Value::String(format!("{broker_node:016X}"));
        ctx.view
            .insert(
                loan_key,
                serde_json::to_vec(&loan).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // 2. Broker: grow DebtTotal, bump LoanSequence and the active-loan count.
        let debt = parse_num(&broker, "DebtTotal");
        broker["DebtTotal"] = serde_json::Value::String(debt.add(&principal).to_decimal_string());
        broker["LoanSequence"] = serde_json::Value::from(loan_sequence + 1);
        let broker_loan_count = broker["OwnerCount"].as_u64().unwrap_or(0) as u32;
        broker["OwnerCount"] = serde_json::Value::from(broker_loan_count + 1);
        ctx.view
            .update(
                broker_key,
                serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // 3. Vault: the principal leaves the available pool.
        vault["AssetsAvailable"] =
            serde_json::Value::String(assets_available.sub(&principal).to_decimal_string());
        ctx.view
            .update(
                vault_key,
                serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // 4. Move the principal from the vault pseudo to the borrower.
        let vp_hold = crate::amm_helpers::iou_holding_number(
            ctx.view,
            &vault_pseudo_id,
            &issuer_id,
            &cur_bytes,
        );
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &vault_pseudo_id,
            &issuer_id,
            &cur_bytes,
            &vp_hold.sub(&principal),
        )?;
        let b_hold =
            crate::amm_helpers::iou_holding_number(ctx.view, &borrower_id, &issuer_id, &cur_bytes);
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &borrower_id,
            &issuer_id,
            &cur_bytes,
            &b_hold.add(&principal),
        )?;

        // 5. Borrower owns the new Loan.
        let borrower_key = keylet::account(&borrower_id);
        let bb = ctx
            .view
            .read(&borrower_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut borrower_acct: serde_json::Value =
            serde_json::from_slice(&bb).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut borrower_acct, 1);
        ctx.view
            .update(
                borrower_key,
                serde_json::to_vec(&borrower_acct).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // 6. Bump the caller's (broker owner's) sequence.
        let acct_key = keylet::account(&account_id);
        let ab = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let account: serde_json::Value =
            serde_json::from_slice(&ab).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(
                acct_key,
                serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}
