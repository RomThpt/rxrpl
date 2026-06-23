use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};

use crate::amm_helpers;
use crate::helpers;
use crate::owner_dir::add_to_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

const LSF_DISABLE_MASTER: u32 = 0x0010_0000;
const LSF_DEFAULT_RIPPLE: u32 = 0x0080_0000;
const LSF_DEPOSIT_AUTH: u32 = 0x0100_0000;
const LSF_LOW_RESERVE: u32 = 0x0001_0000;
const LSF_HIGH_RESERVE: u32 = 0x0002_0000;
const LSF_LOW_NO_RIPPLE: u32 = 0x0010_0000;
const LSF_HIGH_NO_RIPPLE: u32 = 0x0020_0000;
const LSF_AMM_NODE: u32 = 0x0100_0000;

const VOTE_WEIGHT_SCALE_FACTOR: u32 = 100_000;
const TOTAL_TIME_SLOT_SECS: u32 = 86_400;
const AUCTION_SLOT_DISCOUNTED_FEE_FRACTION: u32 = 10;

const ZERO_TXID: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub struct AMMCreateTransactor;

impl Transactor for AMMCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let amount_field = ctx
            .tx
            .get("Amount")
            .ok_or(TransactionResult::TemBadAmount)?;
        let amount2_field = ctx
            .tx
            .get("Amount2")
            .ok_or(TransactionResult::TemBadAmount)?;

        let asset = derive_asset(ctx.tx, "Asset", "Amount")?;
        let asset2 = derive_asset(ctx.tx, "Asset2", "Amount2")?;

        amm_helpers::validate_asset(&asset)?;
        amm_helpers::validate_asset(&asset2)?;

        if !amm_helpers::assets_differ(&asset, &asset2) {
            return Err(TransactionResult::TemMalformed);
        }

        if !amm_helpers::amount_is_positive(amount_field) {
            return Err(TransactionResult::TemBadAmount);
        }
        if !amm_helpers::amount_is_positive(amount2_field) {
            return Err(TransactionResult::TemBadAmount);
        }

        if let Some(fee) = helpers::get_u32_field(ctx.tx, "TradingFee") {
            if fee > 1000 {
                return Err(TransactionResult::TemBadFee);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let (_, account) = helpers::read_account_by_address(ctx.view, account_str)?;

        let amm_key = amm_key_from_tx(ctx.tx)?;
        if ctx.view.exists(&amm_key) {
            return Err(TransactionResult::TecDuplicate);
        }

        let balance = helpers::get_balance(&account);
        let amount_field = ctx
            .tx
            .get("Amount")
            .ok_or(TransactionResult::TemBadAmount)?;
        let amount2_field = ctx
            .tx
            .get("Amount2")
            .ok_or(TransactionResult::TemBadAmount)?;
        let xrp_needed = xrp_drops(amount_field)
            .checked_add(xrp_drops(amount2_field))
            .ok_or(TransactionResult::TemBadAmount)?;
        if balance < xrp_needed {
            return Err(TransactionResult::TecUnfunded);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        use rxrpl_amount::number::Number;

        let account_str = helpers::get_account(ctx.tx)?;
        let creator =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let amount_field = ctx
            .tx
            .get("Amount")
            .cloned()
            .ok_or(TransactionResult::TemBadAmount)?;
        let amount2_field = ctx
            .tx
            .get("Amount2")
            .cloned()
            .ok_or(TransactionResult::TemBadAmount)?;
        let asset = derive_asset(ctx.tx, "Asset", "Amount")?;
        let asset2 = derive_asset(ctx.tx, "Asset2", "Amount2")?;
        let trading_fee = helpers::get_u32_field(ctx.tx, "TradingFee").unwrap_or(0) as u16;

        let amm_key = amm_helpers::compute_amm_key(&asset, &asset2)?;

        // Derive the AMM pseudo-account: loop i=0..256 over
        // ripesha(sha512Half(u16be(i) || parentHash || ammKey)), taking the first
        // i whose account keylet does not already exist.
        let amm_id = derive_pseudo_account(ctx, &amm_key)?;
        let amm_str = encode_account_id(&amm_id);

        // Sorted asset objects (low/high) by (currency, issuer) bytes.
        let (low_asset, high_asset) = sort_asset_objects(&asset, &asset2)?;

        // LP currency = 0x03 || sha512Half(minCur||maxCur)[0..19], issuer = amm_id.
        let lp_currency = amm_helpers::lp_currency_from_assets(&asset, &asset2)?;
        let lp_currency_hex = hex::encode_upper(lp_currency);

        // Initial LP tokens = root2(amount1 * amount2) Downward.
        let amt1_num = amount_to_number(&amount_field)?;
        let amt2_num = amount_to_number(&amount2_field)?;
        let lp_tokens = amm_helpers::amm_lp_tokens(&amt1_num, &amt2_num);

        // 1. Create the AMM pseudo-account root.
        let seq = ctx.view.seq();
        let amm_acct_key = keylet::account(&amm_id);
        let amm_acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": amm_str,
            "Balance": "0",
            "Sequence": seq,
            "OwnerCount": 0,
            "Flags": LSF_DISABLE_MASTER | LSF_DEFAULT_RIPPLE | LSF_DEPOSIT_AUTH,
            "AMMID": hex::encode_upper(amm_key.as_bytes()),
            "PreviousTxnID": ZERO_TXID,
            "PreviousTxnLgrSeq": 0,
        });
        ctx.view
            .insert(amm_acct_key, serde_json::to_vec(&amm_acct).map_err(|_| TransactionResult::TefInternal)?)
            .map_err(|_| TransactionResult::TefInternal)?;

        // 2. Build the AMM entry, linked into the AMM account's owner directory.
        let amm_owner_node = add_to_owner_dir(ctx.view, &amm_id, &amm_key)?;
        let expiration = ctx.view.parent_close_time() + TOTAL_TIME_SLOT_SECS;

        let mut vote_entry = serde_json::json!({
            "Account": account_str,
            "VoteWeight": VOTE_WEIGHT_SCALE_FACTOR,
        });
        if trading_fee != 0 {
            vote_entry["TradingFee"] = serde_json::Value::from(trading_fee);
        }
        let mut auction_slot = serde_json::json!({
            "Account": account_str,
            "Expiration": expiration,
            "Price": {
                "currency": lp_currency_hex,
                "issuer": amm_str,
                "value": "0",
            },
        });
        let dfee = trading_fee as u32 / AUCTION_SLOT_DISCOUNTED_FEE_FRACTION;
        if dfee != 0 {
            auction_slot["DiscountedFee"] = serde_json::Value::from(dfee);
        }

        let mut amm = serde_json::json!({
            "LedgerEntryType": "AMM",
            "Account": amm_str,
            "Asset2": high_asset,
            "LPTokenBalance": {
                "currency": lp_currency_hex,
                "issuer": amm_str,
                "value": lp_tokens.to_decimal_string(),
            },
            "VoteSlots": [ { "VoteEntry": vote_entry } ],
            "AuctionSlot": auction_slot,
            "PreviousTxnID": ZERO_TXID,
            "PreviousTxnLgrSeq": 0,
        });
        // Asset (low) is omitted when XRP (the default STIssue).
        if !is_xrp_asset(&low_asset) {
            amm["Asset"] = low_asset.clone();
        }
        if trading_fee != 0 {
            amm["TradingFee"] = serde_json::Value::from(trading_fee);
        }
        if amm_owner_node != 0 {
            amm["OwnerNode"] = serde_json::Value::String(format!("{amm_owner_node:016X}"));
        }
        ctx.view
            .insert(amm_key, serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?)
            .map_err(|_| TransactionResult::TefInternal)?;

        // 3. Mint LP tokens to the creator: create the LP RippleState line.
        let lp_tokens_num = Number::from_iou(&lp_tokens);
        create_iou_line(
            ctx,
            &creator,
            &amm_id,
            &lp_currency_hex,
            &lp_tokens_num,
            LineKind::Lp,
        )?;

        // 4. Send each asset leg from the creator into the AMM pool.
        let mut xrp_legs: u64 = 0;
        let mut amm_owner_inc: i32 = 0;
        send_leg(ctx, &creator, &amm_id, &amount_field, &mut xrp_legs, &mut amm_owner_inc)?;
        send_leg(ctx, &creator, &amm_id, &amount2_field, &mut xrp_legs, &mut amm_owner_inc)?;

        // Apply the accumulated XRP balance + owner count to the AMM account.
        if xrp_legs != 0 || amm_owner_inc != 0 {
            let bytes = ctx
                .view
                .read(&amm_acct_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut acct: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
            helpers::set_balance(&mut acct, xrp_legs);
            if amm_owner_inc != 0 {
                helpers::adjust_owner_count(&mut acct, amm_owner_inc);
            }
            ctx.view
                .update(amm_acct_key, serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // 5. Finalize the creator's AccountRoot: XRP legs out, bump sequence
        // (the +1 OwnerCount for the LP line was applied when the line was
        // created; the fee is handled by the engine).
        let acct_key = keylet::account(&creator);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let balance = helpers::get_balance(&account);
        helpers::set_balance(&mut account, balance.saturating_sub(xrp_legs));
        helpers::increment_sequence(&mut account);
        ctx.view
            .update(acct_key, serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

/// Send one asset leg from the creator into the AMM pool. XRP legs accumulate
/// into `xrp_legs` (added to the AMM balance and deducted from the creator).
/// IOU legs debit the creator's existing line and create the AMM↔issuer pool
/// line (carrying `lsfAMMNode` + reserve on the AMM side), bumping the AMM
/// owner count.
fn send_leg(
    ctx: &mut ApplyContext<'_>,
    creator: &AccountId,
    amm_id: &AccountId,
    amount: &serde_json::Value,
    xrp_legs: &mut u64,
    amm_owner_inc: &mut i32,
) -> Result<(), TransactionResult> {
    use rxrpl_amount::number::Number;

    if amount.is_string() {
        *xrp_legs = xrp_legs.saturating_add(xrp_drops(amount));
        return Ok(());
    }

    let issuer = decode_account_id(
        amount["issuer"]
            .as_str()
            .ok_or(TransactionResult::TemBadIssuer)?,
    )
    .map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let currency = helpers::currency_to_bytes(amount["currency"].as_str().unwrap_or_default());
    let currency_hex = hex::encode_upper(currency);
    let deposit = Number::from_iou(&amm_helpers::parse_iou_value(
        amount["value"].as_str().unwrap_or("0"),
    ));

    // Debit the creator's existing IOU holding.
    let creator_hold = amm_helpers::iou_holding_number(ctx.view, creator, &issuer, &currency);
    amm_helpers::set_iou_holding(ctx.view, creator, &issuer, &currency, &creator_hold.sub(&deposit))?;

    // Create the AMM↔issuer pool line holding +deposit on the AMM side.
    create_iou_line(
        ctx,
        amm_id,
        &issuer,
        &currency_hex,
        &deposit,
        LineKind::AmmPool,
    )?;
    *amm_owner_inc += 1;

    // Adding the pool line to the issuer's owner directory threads the issuer's
    // AccountRoot (rippled emits it as a ModifiedNode). Re-write it unchanged so
    // its PreviousTxnID is stamped.
    touch_account(ctx, &issuer)?;
    Ok(())
}

enum LineKind {
    /// LP-token line: holder side carries Reserve | NoRipple, holder owner +1.
    Lp,
    /// AMM↔issuer pool line: AMM side carries lsfAMMNode | Reserve (no NoRipple),
    /// owner count handled by the caller (on the AMM account).
    AmmPool,
}

/// Create a `RippleState` line where `holder` holds `+amount` of the IOU issued
/// by `issuer`. The balance is stored from the low account's perspective.
fn create_iou_line(
    ctx: &mut ApplyContext<'_>,
    holder: &AccountId,
    issuer: &AccountId,
    currency_hex: &str,
    amount: &rxrpl_amount::number::Number,
    kind: LineKind,
) -> Result<(), TransactionResult> {
    let cur_bytes: [u8; 20] = hex::decode(currency_hex)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(TransactionResult::TefInternal)?;
    let tl_key = keylet::trust_line(holder, issuer, &cur_bytes);
    let holder_is_low = holder.as_bytes() < issuer.as_bytes();

    let balance = if holder_is_low {
        *amount
    } else {
        amount.negate()
    };

    let holder_limit = serde_json::json!({
        "currency": currency_hex,
        "issuer": encode_account_id(holder),
        "value": "0",
    });
    let issuer_limit = serde_json::json!({
        "currency": currency_hex,
        "issuer": encode_account_id(issuer),
        "value": "0",
    });
    let (low_limit, high_limit) = if holder_is_low {
        (holder_limit, issuer_limit)
    } else {
        (issuer_limit, holder_limit)
    };

    let holder_page = add_to_owner_dir(ctx.view, holder, &tl_key)?;
    let issuer_page = add_to_owner_dir(ctx.view, issuer, &tl_key)?;
    let (low_node, high_node) = if holder_is_low {
        (holder_page, issuer_page)
    } else {
        (issuer_page, holder_page)
    };

    // The reserve flag falls on the holder side. For an LP line the holder side
    // also carries NoRipple; for an AMM pool line it carries lsfAMMNode and no
    // NoRipple.
    let flags = match kind {
        LineKind::Lp => {
            if holder_is_low {
                LSF_LOW_RESERVE | LSF_LOW_NO_RIPPLE
            } else {
                LSF_HIGH_RESERVE | LSF_HIGH_NO_RIPPLE
            }
        }
        LineKind::AmmPool => {
            LSF_AMM_NODE
                | if holder_is_low {
                    LSF_LOW_RESERVE
                } else {
                    LSF_HIGH_RESERVE
                }
        }
    };

    let mut account_one = [0u8; 20];
    account_one[19] = 1;
    let no_account = encode_account_id(&AccountId::from(account_one));
    let mut tl_obj = serde_json::json!({
        "LedgerEntryType": "RippleState",
        "Balance": { "currency": currency_hex, "issuer": no_account, "value": balance.to_iou().to_decimal_string() },
        "LowLimit": low_limit,
        "HighLimit": high_limit,
        "Flags": flags,
        "PreviousTxnID": ZERO_TXID,
        "PreviousTxnLgrSeq": 0,
    });
    if low_node != 0 {
        tl_obj["LowNode"] = serde_json::Value::String(format!("{low_node:016X}"));
    }
    if high_node != 0 {
        tl_obj["HighNode"] = serde_json::Value::String(format!("{high_node:016X}"));
    }
    ctx.view
        .insert(tl_key, serde_json::to_vec(&tl_obj).map_err(|_| TransactionResult::TefInternal)?)
        .map_err(|_| TransactionResult::TefInternal)?;

    // Only the LP line bumps the holder's (creator's) owner count here; the AMM
    // pool line's owner-count bump is applied once on the AMM account by the
    // caller (the AMM pseudo-account isn't updated per-line).
    if let LineKind::Lp = kind {
        let holder_key = keylet::account(holder);
        if let Some(b) = ctx.view.read(&holder_key) {
            let mut acct: serde_json::Value =
                serde_json::from_slice(&b).map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut acct, 1);
            ctx.view
                .update(holder_key, serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?)
                .map_err(|_| TransactionResult::TefInternal)?;
        }
    }
    Ok(())
}

/// Re-write an account root unchanged so the harness threads its PreviousTxnID.
fn touch_account(
    ctx: &mut ApplyContext<'_>,
    account: &AccountId,
) -> Result<(), TransactionResult> {
    let key = keylet::account(account);
    if let Some(bytes) = ctx.view.read(&key) {
        ctx.view
            .update(key, bytes)
            .map_err(|_| TransactionResult::TefInternal)?;
    }
    Ok(())
}

/// Derive the AMM pseudo-account address: the first `i` in `0..256` such that
/// `ripesha(sha512Half(u16be(i) || parentHash || ammKey))` is not an existing
/// account.
fn derive_pseudo_account(
    ctx: &ApplyContext<'_>,
    amm_key: &Hash256,
) -> Result<AccountId, TransactionResult> {
    let parent = ctx.view.parent_hash();
    for i in 0u16..256 {
        let ibe = i.to_be_bytes();
        let hash = rxrpl_crypto::sha512_half::sha512_half(&[
            &ibe,
            parent.as_bytes(),
            amm_key.as_bytes(),
        ]);
        let id = rxrpl_codec::address::classic::account_id_from_public_key(hash.as_bytes());
        if !ctx.view.exists(&keylet::account(&id)) {
            return Ok(id);
        }
    }
    Err(TransactionResult::TecDuplicate)
}

/// Resolve an asset object from a tx, deriving it from the matching Amount when
/// the explicit Asset field is absent.
fn derive_asset(
    tx: &serde_json::Value,
    asset_field: &str,
    amount_field: &str,
) -> Result<serde_json::Value, TransactionResult> {
    tx.get(asset_field)
        .cloned()
        .or_else(|| {
            tx.get(amount_field)
                .and_then(amm_helpers::asset_spec_from_amount)
        })
        .ok_or(TransactionResult::TemMalformed)
}

fn amm_key_from_tx(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let asset = derive_asset(tx, "Asset", "Amount")?;
    let asset2 = derive_asset(tx, "Asset2", "Amount2")?;
    amm_helpers::compute_amm_key(&asset, &asset2)
}

/// Sort two asset objects by (currency, issuer) bytes, returning (low, high).
fn sort_asset_objects(
    a: &serde_json::Value,
    b: &serde_json::Value,
) -> Result<(serde_json::Value, serde_json::Value), TransactionResult> {
    let (ca, ia) = amm_helpers::asset_to_bytes(a)?;
    let (cb, ib) = amm_helpers::asset_to_bytes(b)?;
    if (ca, ia) <= (cb, ib) {
        Ok((a.clone(), b.clone()))
    } else {
        Ok((b.clone(), a.clone()))
    }
}

fn is_xrp_asset(asset: &serde_json::Value) -> bool {
    matches!(asset.as_str(), Some("XRP"))
        || asset
            .get("currency")
            .and_then(|c| c.as_str())
            .map(|c| c == "XRP")
            .unwrap_or(false)
}

/// An Amount field as a `Number` (integer drops for XRP, IOU value otherwise).
fn amount_to_number(
    amount: &serde_json::Value,
) -> Result<rxrpl_amount::number::Number, TransactionResult> {
    use rxrpl_amount::number::Number;
    if let Some(s) = amount.as_str() {
        let drops: i64 = s.parse().map_err(|_| TransactionResult::TemBadAmount)?;
        return Ok(Number::from_int(drops));
    }
    Ok(Number::from_iou(&amm_helpers::parse_iou_value(
        amount["value"].as_str().unwrap_or("0"),
    )))
}

/// XRP drops carried by an Amount field, 0 if it's an IOU.
fn xrp_drops(amount: &serde_json::Value) -> u64 {
    amount
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
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

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const ISSUER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn create_xrp_iou_pool() {
        let mut ledger = setup_accounts();
        // Seed the creator's IOU trust line so the IOU leg can be debited. ALICE
        // holds 100 USD from ISSUER. ALICE < ISSUER so ALICE is low.
        let alice = decode_account_id(ALICE).unwrap();
        let issuer = decode_account_id(ISSUER).unwrap();
        let cur = helpers::currency_to_bytes("USD");
        let tl_key = keylet::trust_line(&alice, &issuer, &cur);
        let low_is_alice = alice.as_bytes() < issuer.as_bytes();
        let bal = if low_is_alice { "100" } else { "-100" };
        let line = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": bal},
            "LowLimit": {"currency":"USD","issuer": if low_is_alice {ALICE} else {ISSUER},"value":"1000"},
            "HighLimit": {"currency":"USD","issuer": if low_is_alice {ISSUER} else {ALICE},"value":"1000"},
            "Flags": 0,
        });
        ledger
            .put_state(tl_key, serde_json::to_vec(&line).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": ISSUER},
            "Amount": "10000000",
            "Amount2": {"currency": "USD", "issuer": ISSUER, "value": "10"},
            "TradingFee": 500,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AMMCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        let amm_bytes = sandbox.read(&amm_key).unwrap();
        let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).unwrap();
        assert_eq!(amm["LedgerEntryType"].as_str().unwrap(), "AMM");
        // XRP (low) asset is omitted; Asset2 is the IOU.
        assert!(amm.get("Asset").is_none());
        assert_eq!(amm["Asset2"]["currency"].as_str().unwrap(), "USD");
        assert_eq!(amm["TradingFee"].as_u64().unwrap(), 500);
        assert!(amm.get("VoteSlots").is_some());
        assert!(amm.get("AuctionSlot").is_some());

        // Creator balance: 100_000_000 - 10_000_000 XRP leg = 90_000_000.
        let acct_bytes = sandbox.read(&keylet::account(&alice)).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["Balance"].as_str().unwrap(), "90000000");
        // +1 OwnerCount for the LP line.
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn reject_same_assets() {
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": "XRP",
            "Amount": "10000000",
            "Amount2": "5000000",
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
            AMMCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": ISSUER},
            "Amount": "0",
            "Amount2": "5000000",
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
            AMMCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn reject_trading_fee_too_high() {
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": ISSUER},
            "Amount": "10000000",
            "Amount2": "5000000",
            "TradingFee": 1001,
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
            AMMCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadFee)
        );
    }

    #[test]
    fn reject_missing_asset2() {
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Amount": "10000000",
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
            AMMCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn reject_insufficient_balance() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": ISSUER},
            "Amount": "80000000",
            "Amount2": "80000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AMMCreateTransactor.preclaim(&ctx),
            Err(TransactionResult::TecUnfunded)
        );
    }

    #[test]
    fn reject_duplicate_amm() {
        let mut ledger = setup_accounts();
        let tx = serde_json::json!({
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": ISSUER},
        });
        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        let amm = serde_json::json!({
            "LedgerEntryType": "AMM",
            "Account": ALICE,
            "LPTokenBalance": {"currency": "03ABCDEF", "issuer": ALICE, "value": "100"},
        });
        ledger
            .put_state(amm_key, serde_json::to_vec(&amm).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": ISSUER},
            "Amount": "10000000",
            "Amount2": "5000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AMMCreateTransactor.preclaim(&ctx),
            Err(TransactionResult::TecDuplicate)
        );
    }
}
