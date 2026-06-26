use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultWithdrawTransactor;

/// Parse the 32-byte `VaultID` (the vault keylet itself).
fn vault_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, "VaultID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    Hash256::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
}

fn num(v: &serde_json::Value, field: &str) -> u128 {
    v.get(field)
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// `a / b` rounded to nearest, ties to even (matching a Number assigned to an
/// integer STAmount).
fn div_round_nearest_even(a: u128, b: u128) -> u128 {
    let q = a / b;
    let r = a % b;
    let twice = r * 2;
    if twice > b || (twice == b && q % 2 == 1) {
        q + 1
    } else {
        q
    }
}

impl Transactor for VaultWithdrawTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        vault_id(ctx.tx)?;
        let amount = ctx
            .tx
            .get("Amount")
            .ok_or(TransactionResult::TemBadAmount)?;
        if amount.is_object() {
            let v = amount.get("value").and_then(|x| x.as_str()).unwrap_or("0");
            if v.trim_start_matches('-').trim_start_matches('0').is_empty() {
                return Err(TransactionResult::TemBadAmount);
            }
        } else {
            let drops = helpers::get_u64_str_field(ctx.tx, "Amount")
                .ok_or(TransactionResult::TemBadAmount)?;
            if drops == 0 {
                return Err(TransactionResult::TemBadAmount);
            }
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

        if ctx.tx.get("Amount").map(|a| a.is_object()).unwrap_or(false) {
            return apply_iou_withdraw(ctx, account_str, &account_id);
        }

        let amount = helpers::get_u64_str_field(ctx.tx, "Amount")
            .ok_or(TransactionResult::TemBadAmount)? as u128;

        let vault_key = vault_id(ctx.tx)?;
        let vault_bytes = ctx
            .view
            .read(&vault_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let pseudo_str = vault["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let pseudo_id =
            decode_account_id(&pseudo_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let owner_str = vault["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();

        let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
        let issuance_bytes = ctx
            .view
            .read(&issuance_key)
            .ok_or(TransactionResult::TefInternal)?;
        let mut issuance: serde_json::Value =
            serde_json::from_slice(&issuance_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let assets_total = num(&vault, "AssetsTotal");
        let assets_available = num(&vault, "AssetsAvailable");
        let loss = num(&vault, "LossUnrealized");
        let shares_total = num(&issuance, "OutstandingAmount");
        // Owners waive the unrealized-loss discount on the share price.
        let effective_total = assets_total - loss;

        if effective_total == 0 || shares_total == 0 {
            return Err(TransactionResult::TecInsufficientFunds);
        }

        // assetsToSharesWithdraw (round to nearest) then sharesToAssetsWithdraw
        // (truncate). A full redemption empties the vault exactly.
        let shares_redeemed = div_round_nearest_even(shares_total * amount, effective_total);
        if shares_redeemed == 0 {
            return Err(TransactionResult::TecPrecisionLoss);
        }
        let assets_withdrawn = effective_total * shares_redeemed / shares_total;

        let is_final = shares_redeemed == shares_total;
        let (new_total, new_available, assets_out) = if is_final {
            (0u128, 0u128, assets_available)
        } else {
            if assets_available < assets_withdrawn {
                return Err(TransactionResult::TecInsufficientFunds);
            }
            (
                assets_total - assets_withdrawn,
                assets_available - assets_withdrawn,
                assets_withdrawn,
            )
        };

        // Burn the redeemed shares from the account's MPToken and the issuance.
        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &account_id);
        let mptoken_bytes = ctx
            .view
            .read(&mptoken_key)
            .ok_or(TransactionResult::TecInsufficientFunds)?;
        let mut mptoken: serde_json::Value =
            serde_json::from_slice(&mptoken_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let held = num(&mptoken, "MPTAmount");
        if held < shares_redeemed {
            return Err(TransactionResult::TecInsufficientFunds);
        }
        let remaining_shares = held - shares_redeemed;

        // rippled removes a non-owner's emptied share holding; the owner keeps it.
        let remove_mptoken = remaining_shares == 0 && account_str != owner_str;
        if remove_mptoken {
            ctx.view
                .erase(&mptoken_key)
                .map_err(|_| TransactionResult::TefInternal)?;
            crate::owner_dir::remove_from_owner_dir(ctx.view, &account_id, &mptoken_key)?;
        } else {
            mptoken["MPTAmount"] = serde_json::Value::String(remaining_shares.to_string());
            ctx.view
                .update(
                    mptoken_key,
                    serde_json::to_vec(&mptoken).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        issuance["OutstandingAmount"] =
            serde_json::Value::String((shares_total - shares_redeemed).to_string());
        ctx.view
            .update(
                issuance_key,
                serde_json::to_vec(&issuance).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Shrink the vault's asset accounting.
        vault["AssetsTotal"] = serde_json::Value::String(new_total.to_string());
        vault["AssetsAvailable"] = serde_json::Value::String(new_available.to_string());
        ctx.view
            .update(
                vault_key,
                serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Move the withdrawn XRP from the pseudo-account back to the account.
        let pseudo_key = keylet::account(&pseudo_id);
        let pseudo_bytes = ctx
            .view
            .read(&pseudo_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut pseudo: serde_json::Value =
            serde_json::from_slice(&pseudo_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let pbal = helpers::get_balance(&pseudo);
        helpers::set_balance(
            &mut pseudo,
            pbal.checked_sub(assets_out as u64)
                .ok_or(TransactionResult::TefInternal)?,
        );
        ctx.view
            .update(
                pseudo_key,
                serde_json::to_vec(&pseudo).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let bal = helpers::get_balance(&account);
        helpers::set_balance(&mut account, bal + assets_out as u64);
        helpers::increment_sequence(&mut account);
        if remove_mptoken {
            helpers::adjust_owner_count(&mut account, -1);
        }
        ctx.view
            .update(
                acct_key,
                serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

use rxrpl_amount::number::Number;

/// Truncate a non-negative Number toward zero into a u128.
fn num_trunc(n: &Number) -> u128 {
    if n.is_zero() {
        return 0;
    }
    let m = n.mantissa() as u128;
    let e = n.exponent();
    if e >= 0 {
        m.saturating_mul(10u128.pow(e as u32))
    } else {
        m / 10u128.pow((-e) as u32)
    }
}

/// Round a non-negative Number to the nearest u128, ties to even.
fn num_round_even(n: &Number) -> u128 {
    if n.is_zero() {
        return 0;
    }
    let e = n.exponent();
    if e >= 0 {
        return num_trunc(n);
    }
    let div = 10u128.pow((-e) as u32);
    let m = n.mantissa() as u128;
    let q = m / div;
    let r = m % div;
    let twice = r * 2;
    if twice > div || (twice == div && q % 2 == 1) {
        q + 1
    } else {
        q
    }
}

/// VaultWithdraw for an IOU single-asset vault: the inverse of the IOU deposit.
fn apply_iou_withdraw(
    ctx: &mut ApplyContext<'_>,
    account_str: &str,
    account_id: &rxrpl_primitives::AccountId,
) -> Result<TransactionResult, TransactionResult> {
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

    let vault_key = vault_id(ctx.tx)?;
    let vault_bytes = ctx
        .view
        .read(&vault_key)
        .ok_or(TransactionResult::TecNoEntry)?;
    let mut vault: serde_json::Value =
        serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

    let pseudo_id = decode_account_id(
        vault["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?,
    )
    .map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let owner_str = vault["Owner"]
        .as_str()
        .ok_or(TransactionResult::TefInternal)?
        .to_string();

    let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
    let issuance_bytes = ctx
        .view
        .read(&issuance_key)
        .ok_or(TransactionResult::TefInternal)?;
    let mut issuance: serde_json::Value =
        serde_json::from_slice(&issuance_bytes).map_err(|_| TransactionResult::TefInternal)?;

    let parse = |v: &serde_json::Value, f: &str| {
        Number::from_iou(&crate::amm_helpers::parse_iou_value(
            v.get(f).and_then(|x| x.as_str()).unwrap_or("0"),
        ))
    };
    let assets_total = parse(&vault, "AssetsTotal");
    let assets_available = parse(&vault, "AssetsAvailable");
    let loss = parse(&vault, "LossUnrealized");
    let shares_total = num(&issuance, "OutstandingAmount");
    let amount_num = Number::from_iou(&crate::amm_helpers::parse_iou_value(value));
    let effective_total = assets_total.sub(&loss);

    if effective_total.is_zero() || shares_total == 0 {
        return Err(TransactionResult::TecInsufficientFunds);
    }

    // assetsToSharesWithdraw (round to nearest) then sharesToAssetsWithdraw.
    let shares_redeemed = num_round_even(
        &Number::from_int(shares_total as i64)
            .mul(&amount_num)
            .div(&effective_total),
    );
    if shares_redeemed == 0 {
        return Err(TransactionResult::TecPrecisionLoss);
    }
    let assets_withdrawn = effective_total
        .mul(&Number::from_int(shares_redeemed as i64))
        .div(&Number::from_int(shares_total as i64));

    let is_final = shares_redeemed == shares_total;
    let (new_total, new_available, assets_out) = if is_final {
        (Number::from_int(0), Number::from_int(0), assets_available)
    } else {
        (
            assets_total.sub(&assets_withdrawn),
            assets_available.sub(&assets_withdrawn),
            assets_withdrawn,
        )
    };

    // Burn the shares from the account's MPToken and the issuance.
    let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), account_id);
    let mptoken_bytes = ctx
        .view
        .read(&mptoken_key)
        .ok_or(TransactionResult::TecInsufficientFunds)?;
    let mut mptoken: serde_json::Value =
        serde_json::from_slice(&mptoken_bytes).map_err(|_| TransactionResult::TefInternal)?;
    let held = num(&mptoken, "MPTAmount");
    if held < shares_redeemed {
        return Err(TransactionResult::TecInsufficientFunds);
    }
    let remaining = held - shares_redeemed;
    let remove_mptoken = remaining == 0 && account_str != owner_str;
    if remove_mptoken {
        ctx.view
            .erase(&mptoken_key)
            .map_err(|_| TransactionResult::TefInternal)?;
        crate::owner_dir::remove_from_owner_dir(ctx.view, account_id, &mptoken_key)?;
    } else {
        mptoken["MPTAmount"] = serde_json::Value::String(remaining.to_string());
        ctx.view
            .update(
                mptoken_key,
                serde_json::to_vec(&mptoken).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
    }

    issuance["OutstandingAmount"] =
        serde_json::Value::String((shares_total - shares_redeemed).to_string());
    ctx.view
        .update(
            issuance_key,
            serde_json::to_vec(&issuance).map_err(|_| TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;

    // Shrink the vault's asset accounting.
    vault["AssetsTotal"] = serde_json::Value::String(new_total.to_decimal_string());
    vault["AssetsAvailable"] = serde_json::Value::String(new_available.to_decimal_string());
    ctx.view
        .update(
            vault_key,
            serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;

    // Move the withdrawn IOU from the pseudo's holding back to the account.
    let pseudo_hold =
        crate::amm_helpers::iou_holding_number(ctx.view, &pseudo_id, &issuer, &currency);
    crate::amm_helpers::set_iou_holding(
        ctx.view,
        &pseudo_id,
        &issuer,
        &currency,
        &pseudo_hold.sub(&assets_out),
    )?;
    let acct_hold =
        crate::amm_helpers::iou_holding_number(ctx.view, account_id, &issuer, &currency);
    crate::amm_helpers::set_iou_holding(
        ctx.view,
        account_id,
        &issuer,
        &currency,
        &acct_hold.add(&assets_out),
    )?;

    // Bump the account's sequence (and owner count if its MPToken was removed).
    let acct_key = keylet::account(account_id);
    let acct_bytes = ctx
        .view
        .read(&acct_key)
        .ok_or(TransactionResult::TerNoAccount)?;
    let mut account: serde_json::Value =
        serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
    helpers::increment_sequence(&mut account);
    if remove_mptoken {
        helpers::adjust_owner_count(&mut account, -1);
    }
    ctx.view
        .update(
            acct_key,
            serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;

    Ok(TransactionResult::TesSuccess)
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
    use rxrpl_ledger::Ledger;

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const PSEUDO: &str = "rG9ckJcta51jT4iYdBiGo7du8MsKh7fzXp";
    const SHARE_ID: &str = "00000001A62B0DE19DFAF4D7C4E59DF8927BFF79FE146246";

    fn setup() -> (Ledger, Hash256) {
        let mut ledger = Ledger::genesis();
        let owner_id = decode_account_id(OWNER).unwrap();
        ledger
            .put_state(
                keylet::account(&owner_id),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AccountRoot", "Account": OWNER,
                    "Balance": "90000000", "Sequence": 5, "OwnerCount": 3, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        let pseudo_id = decode_account_id(PSEUDO).unwrap();
        ledger
            .put_state(
                keylet::account(&pseudo_id),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AccountRoot", "Account": PSEUDO,
                    "Balance": "10000000", "Flags": 26214400, "OwnerCount": 1,
                    "VaultID": "3EBDFD5E1263CFB141881792F91E8DCCA03285B8F7BF609DC29D2391EACC176C",
                }))
                .unwrap(),
            )
            .unwrap();

        let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
        ledger
            .put_state(
                issuance_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "MPTokenIssuance", "Flags": 56, "Issuer": PSEUDO,
                    "Sequence": 1, "OutstandingAmount": "10000000",
                }))
                .unwrap(),
            )
            .unwrap();

        ledger
            .put_state(
                keylet::mptoken(issuance_key.as_bytes(), &owner_id),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "MPToken", "Account": OWNER,
                    "MPTokenIssuanceID": SHARE_ID, "MPTAmount": "10000000",
                }))
                .unwrap(),
            )
            .unwrap();

        let vault_key = keylet::vault(&owner_id, 3);
        ledger
            .put_state(
                vault_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Vault", "Account": PSEUDO, "Owner": OWNER,
                    "Sequence": 3, "ShareMPTID": SHARE_ID, "WithdrawalPolicy": 1,
                    "AssetsTotal": "10000000", "AssetsAvailable": "10000000",
                }))
                .unwrap(),
            )
            .unwrap();
        (ledger, vault_key)
    }

    #[test]
    fn partial_withdraw_burns_shares() {
        let (ledger, vault_key) = setup();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultWithdraw",
            "Account": OWNER,
            "VaultID": hex::encode_upper(vault_key.as_bytes()),
            "Amount": "4000000",
            "Fee": "20",
            "Sequence": 5,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            VaultWithdrawTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        let vault: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&vault_key).unwrap()).unwrap();
        assert_eq!(vault["AssetsTotal"].as_str().unwrap(), "6000000");
        assert_eq!(vault["AssetsAvailable"].as_str().unwrap(), "6000000");

        let pseudo_id = decode_account_id(PSEUDO).unwrap();
        let pseudo: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&pseudo_id)).unwrap()).unwrap();
        assert_eq!(pseudo["Balance"].as_str().unwrap(), "6000000");

        let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
        let issuance: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&issuance_key).unwrap()).unwrap();
        assert_eq!(issuance["OutstandingAmount"].as_str().unwrap(), "6000000");

        let owner_id = decode_account_id(OWNER).unwrap();
        let mptoken: serde_json::Value = serde_json::from_slice(
            &sandbox
                .read(&keylet::mptoken(issuance_key.as_bytes(), &owner_id))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(mptoken["MPTAmount"].as_str().unwrap(), "6000000");
    }
}
