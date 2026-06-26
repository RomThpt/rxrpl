use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultDepositTransactor;

/// Parse the 32-byte `VaultID` (the vault keylet itself).
fn vault_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, "VaultID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    Hash256::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
}

/// A vault whose underlying asset is XRP (the `Asset` field is absent, or is
/// `{"currency":"XRP"}` with no issuer).
fn vault_is_xrp(vault: &serde_json::Value) -> bool {
    match vault.get("Asset") {
        None => true,
        Some(a) => {
            a.get("currency").and_then(|c| c.as_str()) == Some("XRP") && a.get("issuer").is_none()
        }
    }
}

/// Read a Number/decimal field stored as a string, defaulting to 0.
fn num(v: &serde_json::Value, field: &str) -> u128 {
    v.get(field)
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

impl Transactor for VaultDepositTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        vault_id(ctx.tx)?;
        let amount = ctx
            .tx
            .get("Amount")
            .ok_or(TransactionResult::TemBadAmount)?;
        if amount.is_object() {
            // IOU amount: value must be positive.
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
        let depositor_str = helpers::get_account(ctx.tx)?;
        let (_, depositor_acct) = helpers::read_account_by_address(ctx.view, depositor_str)?;

        let vault_key = vault_id(ctx.tx)?;
        let vault_bytes = ctx
            .view
            .read(&vault_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // XRP vaults verify the depositor can cover Amount + fee; IOU balance
        // sufficiency is enforced when the holding is debited.
        if vault_is_xrp(&vault) {
            let amount = helpers::get_u64_str_field(ctx.tx, "Amount")
                .ok_or(TransactionResult::TemBadAmount)?;
            let fee = helpers::get_fee(ctx.tx);
            let balance = helpers::get_balance(&depositor_acct);
            if balance < amount + fee {
                return Err(TransactionResult::TecUnfundedPayment);
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let depositor_str = helpers::get_account(ctx.tx)?;
        let depositor_id =
            decode_account_id(depositor_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        if ctx.tx.get("Amount").map(|a| a.is_object()).unwrap_or(false) {
            return apply_iou_deposit(ctx, depositor_str, &depositor_id);
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
        let share_id = vault["ShareMPTID"]
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
        let shares_total = num(&issuance, "OutstandingAmount");

        // assetsToSharesDeposit / sharesToAssetsDeposit for an XRP vault (scale 0):
        // the first deposit mints 1:1, later deposits scale by the pool ratio
        // (truncated toward zero).
        let (shares, assets_deposited) = if shares_total == 0 || assets_total == 0 {
            (amount, amount)
        } else {
            let s = shares_total * amount / assets_total;
            if s == 0 {
                return Err(TransactionResult::TecPrecisionLoss);
            }
            (s, assets_total * s / shares_total)
        };

        // Grow the vault's asset accounting.
        vault["AssetsTotal"] =
            serde_json::Value::String((assets_total + assets_deposited).to_string());
        vault["AssetsAvailable"] =
            serde_json::Value::String((assets_available + assets_deposited).to_string());
        let maximum = num(&vault, "AssetsMaximum");
        if maximum != 0 && assets_total + assets_deposited > maximum {
            return Err(TransactionResult::TecLimitExceeded);
        }
        ctx.view
            .update(
                vault_key,
                serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Move the deposited XRP into the vault's pseudo-account.
        let pseudo_key = keylet::account(&pseudo_id);
        let pseudo_bytes = ctx
            .view
            .read(&pseudo_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut pseudo: serde_json::Value =
            serde_json::from_slice(&pseudo_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let pbal = helpers::get_balance(&pseudo);
        helpers::set_balance(&mut pseudo, pbal + assets_deposited as u64);
        ctx.view
            .update(
                pseudo_key,
                serde_json::to_vec(&pseudo).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Mint the shares to the depositor's MPToken (created on first deposit).
        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &depositor_id);
        let created_mptoken = !ctx.view.exists(&mptoken_key);
        if created_mptoken {
            let mptoken = serde_json::json!({
                "LedgerEntryType": "MPToken",
                "Account": depositor_str,
                "MPTokenIssuanceID": share_id,
                "MPTAmount": shares.to_string(),
                "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
                "PreviousTxnLgrSeq": 0,
            });
            ctx.view
                .insert(
                    mptoken_key,
                    serde_json::to_vec(&mptoken).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
            crate::owner_dir::add_to_owner_dir(ctx.view, &depositor_id, &mptoken_key)?;
        } else {
            let mptoken_bytes = ctx
                .view
                .read(&mptoken_key)
                .ok_or(TransactionResult::TefInternal)?;
            let mut mptoken: serde_json::Value = serde_json::from_slice(&mptoken_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
            let cur = num(&mptoken, "MPTAmount");
            mptoken["MPTAmount"] = serde_json::Value::String((cur + shares).to_string());
            ctx.view
                .update(
                    mptoken_key,
                    serde_json::to_vec(&mptoken).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Track the new total outstanding shares on the issuance.
        issuance["OutstandingAmount"] =
            serde_json::Value::String((shares_total + shares).to_string());
        ctx.view
            .update(
                issuance_key,
                serde_json::to_vec(&issuance).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Debit the depositor's XRP and bump its sequence; if a new MPToken was
        // created, account for the extra owned object.
        let depositor_key = keylet::account(&depositor_id);
        let depositor_bytes = ctx
            .view
            .read(&depositor_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut depositor_acct: serde_json::Value =
            serde_json::from_slice(&depositor_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let balance = helpers::get_balance(&depositor_acct);
        helpers::set_balance(
            &mut depositor_acct,
            balance
                .checked_sub(assets_deposited as u64)
                .ok_or(TransactionResult::TecUnfundedPayment)?,
        );
        helpers::increment_sequence(&mut depositor_acct);
        if created_mptoken {
            helpers::adjust_owner_count(&mut depositor_acct, 1);
        }
        ctx.view
            .update(
                depositor_key,
                serde_json::to_vec(&depositor_acct).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

use rxrpl_amount::number::Number;

/// Truncate a non-negative Number toward zero into a u128.
fn num_to_u128_trunc(n: &Number) -> u128 {
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

/// VaultDeposit for an IOU single-asset vault: shares are priced with the
/// vault's decimal scale, the deposited IOU moves from the depositor's trust
/// line to the pseudo-account's, and the shares are minted as MPT.
fn apply_iou_deposit(
    ctx: &mut ApplyContext<'_>,
    depositor_str: &str,
    depositor_id: &rxrpl_primitives::AccountId,
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
    let share_id = vault["ShareMPTID"]
        .as_str()
        .ok_or(TransactionResult::TefInternal)?
        .to_string();
    let scale = vault.get("Scale").and_then(|s| s.as_u64()).unwrap_or(6) as u32;

    let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
    let issuance_bytes = ctx
        .view
        .read(&issuance_key)
        .ok_or(TransactionResult::TefInternal)?;
    let mut issuance: serde_json::Value =
        serde_json::from_slice(&issuance_bytes).map_err(|_| TransactionResult::TefInternal)?;

    let assets_total = Number::from_iou(&crate::amm_helpers::parse_iou_value(
        vault
            .get("AssetsTotal")
            .and_then(|v| v.as_str())
            .unwrap_or("0"),
    ));
    let assets_available = Number::from_iou(&crate::amm_helpers::parse_iou_value(
        vault
            .get("AssetsAvailable")
            .and_then(|v| v.as_str())
            .unwrap_or("0"),
    ));
    let shares_total = num(&issuance, "OutstandingAmount");
    let amount_num = Number::from_iou(&crate::amm_helpers::parse_iou_value(value));
    let ten_pow_scale = Number::from_int(10i64.pow(scale));

    // assetsToSharesDeposit / sharesToAssetsDeposit with the vault scale: the
    // first deposit mints amount * 10^scale, later deposits scale by the ratio.
    let (shares, assets_deposited) = if shares_total == 0 || assets_total.is_zero() {
        (
            num_to_u128_trunc(&amount_num.mul(&ten_pow_scale)),
            amount_num,
        )
    } else {
        let s = num_to_u128_trunc(
            &Number::from_int(shares_total as i64)
                .mul(&amount_num)
                .div(&assets_total),
        );
        if s == 0 {
            return Err(TransactionResult::TecPrecisionLoss);
        }
        let assets = assets_total
            .mul(&Number::from_int(s as i64))
            .div(&Number::from_int(shares_total as i64));
        (s, assets)
    };

    // Grow the vault's asset accounting (STNumber).
    vault["AssetsTotal"] =
        serde_json::Value::String(assets_total.add(&assets_deposited).to_decimal_string());
    vault["AssetsAvailable"] =
        serde_json::Value::String(assets_available.add(&assets_deposited).to_decimal_string());
    ctx.view
        .update(
            vault_key,
            serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;

    // Move the deposited IOU from the depositor's holding to the pseudo's.
    let depositor_hold =
        crate::amm_helpers::iou_holding_number(ctx.view, depositor_id, &issuer, &currency);
    crate::amm_helpers::set_iou_holding(
        ctx.view,
        depositor_id,
        &issuer,
        &currency,
        &depositor_hold.sub(&assets_deposited),
    )?;
    let pseudo_hold =
        crate::amm_helpers::iou_holding_number(ctx.view, &pseudo_id, &issuer, &currency);
    crate::amm_helpers::set_iou_holding(
        ctx.view,
        &pseudo_id,
        &issuer,
        &currency,
        &pseudo_hold.add(&assets_deposited),
    )?;

    // Mint the shares to the depositor's MPToken (created on first deposit).
    let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), depositor_id);
    let created_mptoken = !ctx.view.exists(&mptoken_key);
    if created_mptoken {
        let mptoken = serde_json::json!({
            "LedgerEntryType": "MPToken",
            "Account": depositor_str,
            "MPTokenIssuanceID": share_id,
            "MPTAmount": shares.to_string(),
            "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
            "PreviousTxnLgrSeq": 0,
        });
        ctx.view
            .insert(
                mptoken_key,
                serde_json::to_vec(&mptoken).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
        crate::owner_dir::add_to_owner_dir(ctx.view, depositor_id, &mptoken_key)?;
    } else {
        let mptoken_bytes = ctx
            .view
            .read(&mptoken_key)
            .ok_or(TransactionResult::TefInternal)?;
        let mut mptoken: serde_json::Value =
            serde_json::from_slice(&mptoken_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let cur = num(&mptoken, "MPTAmount");
        mptoken["MPTAmount"] = serde_json::Value::String((cur + shares).to_string());
        ctx.view
            .update(
                mptoken_key,
                serde_json::to_vec(&mptoken).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
    }

    issuance["OutstandingAmount"] = serde_json::Value::String((shares_total + shares).to_string());
    ctx.view
        .update(
            issuance_key,
            serde_json::to_vec(&issuance).map_err(|_| TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;

    // Bump the depositor's sequence (and owner count if a new MPToken was made).
    let depositor_key = keylet::account(depositor_id);
    let depositor_bytes = ctx
        .view
        .read(&depositor_key)
        .ok_or(TransactionResult::TerNoAccount)?;
    let mut depositor_acct: serde_json::Value =
        serde_json::from_slice(&depositor_bytes).map_err(|_| TransactionResult::TefInternal)?;
    helpers::increment_sequence(&mut depositor_acct);
    if created_mptoken {
        helpers::adjust_owner_count(&mut depositor_acct, 1);
    }
    ctx.view
        .update(
            depositor_key,
            serde_json::to_vec(&depositor_acct).map_err(|_| TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;

    Ok(TransactionResult::TesSuccess)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreflightContext};
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
                    "LedgerEntryType": "AccountRoot",
                    "Account": OWNER,
                    "Balance": "100000000",
                    "Sequence": 4,
                    "OwnerCount": 3,
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        let pseudo_id = decode_account_id(PSEUDO).unwrap();
        ledger
            .put_state(
                keylet::account(&pseudo_id),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AccountRoot",
                    "Account": PSEUDO,
                    "Balance": "0",
                    "Flags": 26214400,
                    "OwnerCount": 1,
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
                    "LedgerEntryType": "MPTokenIssuance",
                    "Flags": 56,
                    "Issuer": PSEUDO,
                    "Sequence": 1,
                }))
                .unwrap(),
            )
            .unwrap();

        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &owner_id);
        ledger
            .put_state(
                mptoken_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "MPToken",
                    "Account": OWNER,
                    "MPTokenIssuanceID": SHARE_ID,
                }))
                .unwrap(),
            )
            .unwrap();

        let owner_id2 = decode_account_id(OWNER).unwrap();
        let vault_key = keylet::vault(&owner_id2, 3);
        ledger
            .put_state(
                vault_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Vault",
                    "Account": PSEUDO,
                    "Owner": OWNER,
                    "Sequence": 3,
                    "ShareMPTID": SHARE_ID,
                    "WithdrawalPolicy": 1,
                }))
                .unwrap(),
            )
            .unwrap();
        (ledger, vault_key)
    }

    #[test]
    fn first_deposit_mints_one_to_one() {
        let (ledger, vault_key) = setup();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultDeposit",
            "Account": OWNER,
            "VaultID": hex::encode_upper(vault_key.as_bytes()),
            "Amount": "10000000",
            "Fee": "20",
            "Sequence": 4,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            VaultDepositTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        let vault: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&vault_key).unwrap()).unwrap();
        assert_eq!(vault["AssetsTotal"].as_str().unwrap(), "10000000");
        assert_eq!(vault["AssetsAvailable"].as_str().unwrap(), "10000000");

        let pseudo_id = decode_account_id(PSEUDO).unwrap();
        let pseudo: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&pseudo_id)).unwrap()).unwrap();
        assert_eq!(pseudo["Balance"].as_str().unwrap(), "10000000");

        let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
        let issuance: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&issuance_key).unwrap()).unwrap();
        assert_eq!(issuance["OutstandingAmount"].as_str().unwrap(), "10000000");

        let owner_id = decode_account_id(OWNER).unwrap();
        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &owner_id);
        let mptoken: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&mptoken_key).unwrap()).unwrap();
        assert_eq!(mptoken["MPTAmount"].as_str().unwrap(), "10000000");
    }

    #[test]
    fn reject_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "VaultDeposit",
            "Account": OWNER,
            "VaultID": "00000000000000000000000000000000000000000000000000000000000000FF",
            "Amount": "0",
            "Fee": "20",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            VaultDepositTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }
}
