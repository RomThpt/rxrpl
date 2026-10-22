use rxrpl_amount::number::Number;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultClawbackTransactor;

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

fn parse_num(v: &serde_json::Value, f: &str) -> Number {
    Number::from_iou(&crate::amm_helpers::parse_iou_value(
        v.get(f).and_then(|x| x.as_str()).unwrap_or("0"),
    ))
}

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

impl Transactor for VaultClawbackTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        vault_id(ctx.tx)?;
        helpers::get_str_field(ctx.tx, "Holder").ok_or(TransactionResult::TemMalformed)?;
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
        let holder_str = helpers::get_str_field(ctx.tx, "Holder")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let holder_id =
            decode_account_id(&holder_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

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

        // Clawback only applies to IOU vaults; the asset's issuer is the caller.
        let asset = vault
            .get("Asset")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let currency = helpers::currency_to_bytes(
            asset
                .get("currency")
                .and_then(|c| c.as_str())
                .ok_or(TransactionResult::TemDisabled)?,
        );
        let issuer_id = decode_account_id(
            asset
                .get("issuer")
                .and_then(|i| i.as_str())
                .ok_or(TransactionResult::TemBadIssuer)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        if issuer_id != account_id {
            return Err(TransactionResult::TecNoPermission);
        }

        let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
        let issuance_bytes = ctx
            .view
            .read(&issuance_key)
            .ok_or(TransactionResult::TefInternal)?;
        let mut issuance: serde_json::Value =
            serde_json::from_slice(&issuance_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let assets_total = parse_num(&vault, "AssetsTotal");
        let assets_available = parse_num(&vault, "AssetsAvailable");
        let loss = parse_num(&vault, "LossUnrealized");
        let shares_total = num(&issuance, "OutstandingAmount");
        let effective_total = assets_total.sub(&loss);

        if effective_total.is_zero() || shares_total == 0 {
            return Err(TransactionResult::TecInsufficientFunds);
        }

        // The holder's current share balance bounds the clawback.
        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &holder_id);
        let mptoken_bytes = ctx
            .view
            .read(&mptoken_key)
            .ok_or(TransactionResult::TecInsufficientFunds)?;
        let mut mptoken: serde_json::Value =
            serde_json::from_slice(&mptoken_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let held_shares = num(&mptoken, "MPTAmount");

        // assetsToClawback: a given Amount maps to shares (round to nearest) then
        // back to assets (truncate); an omitted Amount claws the holder's whole
        // position.
        let amount = ctx.tx.get("Amount");
        let (shares_destroyed, assets_recovered) = match amount {
            Some(a) if a.is_object() => {
                let amount_num = Number::from_iou(&crate::amm_helpers::parse_iou_value(
                    a.get("value").and_then(|v| v.as_str()).unwrap_or("0"),
                ));
                let s = num_round_even(
                    &Number::from_int(shares_total as i64)
                        .mul(&amount_num)
                        .div(&effective_total),
                );
                let s = s.min(held_shares);
                let assets = effective_total
                    .mul(&Number::from_int(s as i64))
                    .div(&Number::from_int(shares_total as i64));
                (s, assets)
            }
            _ => {
                let s = held_shares;
                let assets = effective_total
                    .mul(&Number::from_int(s as i64))
                    .div(&Number::from_int(shares_total as i64));
                (s, assets)
            }
        };
        if shares_destroyed == 0 {
            return Err(TransactionResult::TecPrecisionLoss);
        }
        // Recovered assets cannot exceed what is available.
        let assets_recovered = if num_trunc(&assets_recovered) > num_trunc(&assets_available) {
            assets_available
        } else {
            assets_recovered
        };

        // Shrink the vault's asset accounting.
        vault["AssetsTotal"] =
            serde_json::Value::String(assets_total.sub(&assets_recovered).to_decimal_string());
        vault["AssetsAvailable"] =
            serde_json::Value::String(assets_available.sub(&assets_recovered).to_decimal_string());
        ctx.view
            .update(
                vault_key,
                serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Destroy the holder's shares.
        let remaining = held_shares - shares_destroyed;
        let remove_mptoken = remaining == 0 && holder_str != owner_str;
        if remove_mptoken {
            ctx.view
                .erase(&mptoken_key)
                .map_err(|_| TransactionResult::TefInternal)?;
            crate::owner_dir::remove_from_owner_dir(ctx.view, &holder_id, &mptoken_key)?;
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
            serde_json::Value::String((shares_total - shares_destroyed).to_string());
        ctx.view
            .update(
                issuance_key,
                serde_json::to_vec(&issuance).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Recover the assets: the issuer claws them out of the pseudo-account's
        // holding (since the caller is the asset issuer, this burns the IOU).
        let pseudo_hold =
            crate::amm_helpers::iou_holding_number(ctx.view, &pseudo_id, &issuer_id, &currency);
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &pseudo_id,
            &issuer_id,
            &currency,
            &pseudo_hold.sub(&assets_recovered),
        )?;

        // If the holder's emptied share holding was removed, drop its owner count.
        if remove_mptoken {
            let holder_key = keylet::account(&holder_id);
            if let Some(hb) = ctx.view.read(&holder_key) {
                let mut holder_acct: serde_json::Value =
                    serde_json::from_slice(&hb).map_err(|_| TransactionResult::TefInternal)?;
                helpers::adjust_owner_count(&mut holder_acct, -1);
                ctx.view
                    .update(
                        holder_key,
                        serde_json::to_vec(&holder_acct)
                            .map_err(|_| TransactionResult::TefInternal)?,
                    )
                    .map_err(|_| TransactionResult::TefInternal)?;
            }
        }

        // Bump the issuer's sequence.
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(
                acct_key,
                serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}
