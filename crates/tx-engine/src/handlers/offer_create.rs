use rxrpl_amendment::feature::feature_id;
use rxrpl_amount::{IOUAmount, offer_quality};
use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::helpers;
use crate::owner_dir::{add_to_book_dir, add_to_owner_dir};
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

fn u64_hex(n: u64) -> String {
    format!("{n:016X}")
}

/// The order-book directory index for a given quality: the book base with its
/// low 64 bits replaced by the quality (rate).
fn book_dir_with_quality(book_base: &Hash256, quality: u64) -> Hash256 {
    let mut bytes = *book_base.as_bytes();
    bytes[24..32].copy_from_slice(&quality.to_be_bytes());
    Hash256::new(bytes)
}

/// Parse a decimal value string (e.g. `"277.167203027"`) into an `IOUAmount`,
/// normalising the mantissa into rippled's `[10^15, 10^16)` range.
fn iou_from_decimal(s: &str) -> Option<IOUAmount> {
    let negative = s.starts_with('-');
    let s = s.trim_start_matches(['-', '+']);
    let (int_part, frac_part) = s.split_once('.').unwrap_or((s, ""));
    let digits = format!("{int_part}{frac_part}");
    let mut mantissa: u128 = digits.parse().ok()?;
    if mantissa == 0 {
        return IOUAmount::from_parts(0, 0, false).ok();
    }
    let mut exponent: i32 = -(frac_part.len() as i32);
    while mantissa < 1_000_000_000_000_000 {
        mantissa *= 10;
        exponent -= 1;
    }
    while mantissa >= 10_000_000_000_000_000 {
        mantissa /= 10;
        exponent += 1;
    }
    IOUAmount::from_parts(mantissa as u64, exponent, negative).ok()
}

/// Convert a TakerPays/TakerGets amount (IOU object or XRP drops string) to an
/// `IOUAmount` for quality computation.
fn amount_to_iou(amount: &Value) -> Option<IOUAmount> {
    if let Some(drops) = amount.as_str() {
        // XRP: drops as an integer value.
        return iou_from_decimal(drops);
    }
    iou_from_decimal(amount.get("value").and_then(|v| v.as_str())?)
}

/// The offer's quality (rate) = TakerPays / TakerGets, encoded as rippled's
/// 64-bit rate. Returns 0 if either side cannot be parsed.
fn offer_book_quality(taker_pays: &Value, taker_gets: &Value) -> u64 {
    match (amount_to_iou(taker_pays), amount_to_iou(taker_gets)) {
        (Some(p), Some(g)) => offer_quality(&p, &g).unwrap_or(0),
        _ => 0,
    }
}

/// OfferCreate transaction handler.
///
/// Places an order on the decentralized exchange.
pub struct OfferCreateTransactor;

impl Transactor for OfferCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if ctx.tx.get("TakerPays").is_none() {
            return Err(TransactionResult::TemBadOffer);
        }
        if ctx.tx.get("TakerGets").is_none() {
            return Err(TransactionResult::TemBadOffer);
        }

        // Cannot have both sides be XRP
        let pays_is_xrp = ctx.tx["TakerPays"].is_string();
        let gets_is_xrp = ctx.tx["TakerGets"].is_string();
        if pays_is_xrp && gets_is_xrp {
            return Err(TransactionResult::TemBadOffer);
        }

        // Amounts must be positive
        if pays_is_xrp {
            let amount: u64 = ctx.tx["TakerPays"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if amount == 0 {
                return Err(TransactionResult::TemBadOffer);
            }
        }
        if gets_is_xrp {
            let amount: u64 = ctx.tx["TakerGets"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if amount == 0 {
                return Err(TransactionResult::TemBadOffer);
            }
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

        // PermissionedDEX: if the amendment is enabled and an IOU asset's
        // issuer has the lsfPermissionedDEX flag set, verify the trader
        // holds accepted credentials from the issuer's PermissionedDomain.
        if ctx.rules.enabled(&feature_id("PermissionedDEX")) {
            check_permissioned_asset(ctx, &account_id, ctx.tx.get("TakerPays"))?;
            check_permissioned_asset(ctx, &account_id, ctx.tx.get("TakerGets"))?;
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let acct_key = keylet::account(&account_id);

        // Read account
        let bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut acct: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;

        let sequence = helpers::get_sequence(&acct);

        let (pays_currency, pays_issuer) = currency_and_issuer(&ctx.tx["TakerPays"]);
        let (gets_currency, gets_issuer) = currency_and_issuer(&ctx.tx["TakerGets"]);

        // Consume the sequence up front: rippled charges it (and the fee) even
        // when the transaction ends in a tec claim below.
        crate::owner_dir::consume_seq_or_ticket(ctx.view, &account_id, &mut acct, ctx.tx)?;

        // Cross against the inverse book — existing offers where someone pays
        // our `TakerGets` to receive our `TakerPays` (book keyed by `(gets,
        // pays)`) — best price first, filling crossable offers and reaping
        // unfunded ones (rippled's Taker). Returns the taker's leftover.
        let inverse_book =
            keylet::book_dir(&gets_currency, &gets_issuer, &pays_currency, &pays_issuer);
        let (remaining_pays, remaining_gets, crossed) = cross_offers(
            ctx,
            &account_id,
            &mut acct,
            &ctx.tx["TakerPays"].clone(),
            &ctx.tx["TakerGets"].clone(),
            &inverse_book,
        )?;

        // Nothing left to place when either side is exhausted (fully crossed):
        // rippled places no resting offer. Commit the taker's mutations.
        if value_is_zero(&remaining_pays) || value_is_zero(&remaining_gets) {
            let nb = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .update(acct_key, nb)
                .map_err(|_| TransactionResult::TemMalformed)?;
            return Ok(TransactionResult::TesSuccess);
        }

        // Owner reserve: a resting offer needs reserve for one more owned
        // object. rippled returns tecINSUF_RESERVE_OFFER — fee and sequence
        // charged, no offer placed — when the account cannot afford it AND
        // nothing crossed (a crossing offer may still place below reserve).
        let owner_count = acct.get("OwnerCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if !crossed && helpers::get_balance(&acct) < ctx.fees.account_reserve(owner_count + 1) {
            let nb = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .update(acct_key, nb)
                .map_err(|_| TransactionResult::TemMalformed)?;
            return Ok(TransactionResult::TecInsufReserveOffer);
        }

        let offer_key = keylet::offer(&account_id, sequence);

        // The order-book directory is keyed by the book base (currencies +
        // issuers) with its low 64 bits replaced by the offer's quality (rate),
        // so offers sort by price. rippled stores this as the offer's
        // BookDirectory and tags the directory with the rate + book assets.
        let quality = offer_book_quality(&ctx.tx["TakerPays"], &ctx.tx["TakerGets"]);
        let book_base =
            keylet::book_dir(&pays_currency, &pays_issuer, &gets_currency, &gets_issuer);
        let book_dir_key = book_dir_with_quality(&book_base, quality);
        let book_describe = [
            ("ExchangeRate", Value::from(u64_hex(quality))),
            ("TakerPaysCurrency", hex::encode_upper(pays_currency).into()),
            (
                "TakerPaysIssuer",
                hex::encode_upper(pays_issuer.as_bytes()).into(),
            ),
            ("TakerGetsCurrency", hex::encode_upper(gets_currency).into()),
            (
                "TakerGetsIssuer",
                hex::encode_upper(gets_issuer.as_bytes()).into(),
            ),
        ];
        let book_node = add_to_book_dir(ctx.view, &book_dir_key, &offer_key, &book_describe)?;
        let owner_node = add_to_owner_dir(ctx.view, &account_id, &offer_key)?;

        // Build the Offer SLE. U64 page fields and Flags are omitted when zero,
        // matching rippled's serialization.
        let mut offer = serde_json::Map::new();
        offer.insert("LedgerEntryType".into(), "Offer".into());
        offer.insert("Account".into(), account_str.into());
        offer.insert("Sequence".into(), Value::from(sequence));
        // Resting offer carries the LEFTOVER after crossing, at the original rate.
        offer.insert("TakerPays".into(), remaining_pays.clone());
        offer.insert("TakerGets".into(), remaining_gets.clone());
        offer.insert("BookDirectory".into(), book_dir_key.to_string().into());
        // Placeholder PreviousTxnID/LgrSeq — the engine's central stamping fills
        // these with the creating transaction's id and ledger after apply.
        offer.insert(
            "PreviousTxnID".into(),
            "0000000000000000000000000000000000000000000000000000000000000000".into(),
        );
        offer.insert("PreviousTxnLgrSeq".into(), Value::from(0u32));
        // rippled's Offer always carries Flags, BookNode and OwnerNode, even
        // when zero.
        let flags = ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0);
        offer.insert("Flags".into(), Value::from(flags));
        offer.insert("BookNode".into(), u64_hex(book_node).into());
        offer.insert("OwnerNode".into(), u64_hex(owner_node).into());
        let offer_bytes = serde_json::to_vec(&Value::Object(offer))
            .map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .insert(offer_key, offer_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        // Bump owner count for the new resting offer (sequence already consumed).
        helpers::adjust_owner_count(&mut acct, 1);

        let new_bytes = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .update(acct_key, new_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        Ok(TransactionResult::TesSuccess)
    }
}

/// One side of an offer: native XRP (drops) or an IOU with its issuer/currency.
#[derive(Clone)]
struct Leg {
    is_xrp: bool,
    drops: i64,
    iou: IOUAmount,
    currency: [u8; 20],
    issuer: AccountId,
}

impl Leg {
    fn parse(v: &Value) -> Option<Leg> {
        if let Some(s) = v.as_str() {
            return Some(Leg {
                is_xrp: true,
                drops: s.parse().ok()?,
                iou: IOUAmount::ZERO,
                currency: [0u8; 20],
                issuer: AccountId::from([0u8; 20]),
            });
        }
        let value = v.get("value")?.as_str()?;
        let (currency, issuer) = currency_and_issuer(v);
        Some(Leg {
            is_xrp: false,
            drops: 0,
            iou: IOUAmount::from_decimal_string(value).ok()?,
            currency,
            issuer,
        })
    }

    /// Render this leg with `amount` as its value (an IOU keeps currency/issuer).
    fn with_amount(&self, amount: &IOUAmount, xrp_drops: i64) -> Value {
        if self.is_xrp {
            return Value::from(xrp_drops.to_string());
        }
        serde_json::json!({
            "currency": currency_code_str(&self.currency),
            "issuer": encode_account_id(&self.issuer),
            "value": amount.to_decimal_string(),
        })
    }

    fn is_zero(&self) -> bool {
        if self.is_xrp { self.drops == 0 } else { self.iou.is_zero() }
    }
}

/// True when a TakerPays/TakerGets value is zero (XRP `"0"` or IOU value 0).
fn value_is_zero(v: &Value) -> bool {
    Leg::parse(v).map(|l| l.is_zero()).unwrap_or(true)
}

/// Render a 20-byte currency code back to its string form (3-char ASCII or
/// 40-char hex), the inverse of `helpers::currency_to_bytes`.
fn currency_code_str(currency: &[u8; 20]) -> String {
    let ascii = &currency[12..15];
    let is_standard = currency[..12].iter().all(|&b| b == 0)
        && currency[15..].iter().all(|&b| b == 0)
        && ascii.iter().all(|&b| b != 0);
    if is_standard {
        String::from_utf8_lossy(ascii).to_string()
    } else {
        hex::encode_upper(currency)
    }
}

/// The issuer's transfer rate as an `IOUAmount` multiplier (1.0 = no fee).
/// rippled charges the fee only when neither party is the issuer.
fn transfer_rate(ctx: &ApplyContext<'_>, issuer: &AccountId) -> IOUAmount {
    let one = IOUAmount::from_parts(1_000_000_000, -9, false).unwrap();
    let key = keylet::account(issuer);
    let rate = ctx
        .view
        .read(&key)
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|a| a.get("TransferRate").and_then(|v| v.as_u64()));
    match rate {
        Some(r) if r > 1_000_000_000 => IOUAmount::from_parts(r, -9, false).unwrap_or(one),
        _ => one,
    }
}

/// Apply a signed IOU change to a holder's trust line toward `issuer`.
/// `gain` > 0 credits the holder, < 0 debits. Balance is stored from the low
/// account's perspective, so a high-account holder's balance moves opposite to
/// its gain. Byte-exact via `IOUAmount` (no floating point).
fn credit_line(
    ctx: &mut ApplyContext<'_>,
    holder: &AccountId,
    issuer: &AccountId,
    currency: &[u8; 20],
    gain: &IOUAmount,
) -> Result<(), TransactionResult> {
    let key = keylet::trust_line(holder, issuer, currency);
    let bytes = ctx.view.read(&key).ok_or(TransactionResult::TecPathDry)?;
    let mut line: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let cur = line
        .get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let balance = IOUAmount::from_decimal_string(cur).map_err(|_| TransactionResult::TefInternal)?;
    let holder_is_high = holder.as_bytes() > issuer.as_bytes();
    let delta = if holder_is_high { gain.negate() } else { *gain };
    let new = IOUAmount::add(&balance, &delta).map_err(|_| TransactionResult::TefInternal)?;
    line["Balance"]["value"] = Value::String(new.to_decimal_string());
    let nb = serde_json::to_vec(&line).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(key, nb)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
}

/// Add `delta` drops to an account's XRP balance (read/modify/write the SLE).
fn credit_xrp(
    ctx: &mut ApplyContext<'_>,
    account: &AccountId,
    delta: i64,
) -> Result<(), TransactionResult> {
    let key = keylet::account(account);
    let bytes = ctx.view.read(&key).ok_or(TransactionResult::TecPathDry)?;
    let mut acct: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let bal = helpers::get_balance(&acct) as i64 + delta;
    if bal < 0 {
        return Err(TransactionResult::TecUnfundedOffer);
    }
    helpers::set_balance(&mut acct, bal as u64);
    let nb = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(key, nb)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
}

/// Cross the new offer against the inverse book, filling crossable resting
/// offers best-price-first. Returns the taker's remaining `(TakerPays,
/// TakerGets)` and whether any fill occurred. The taker's own AccountRoot XRP
/// balance is mutated through `taker_acct` (written by the caller); every other
/// entry (counterparties, trust lines, consumed offers) goes through the view.
///
/// Scope: full takes of funded, crossable offers (the validated path, mainnet
/// #338500). Partial takes rescale at the resting offer's quality. Unfunded
/// offers encountered in the walk are reaped, mirroring rippled's dirAdvance.
fn cross_offers(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    taker_pays: &Value,
    taker_gets: &Value,
    inverse_book: &rxrpl_primitives::Hash256,
) -> Result<(Value, Value, bool), TransactionResult> {
    let out_leg = match Leg::parse(taker_pays) {
        Some(l) => l,
        None => return Ok((taker_pays.clone(), taker_gets.clone(), false)),
    };
    let in_leg = match Leg::parse(taker_gets) {
        Some(l) => l,
        None => return Ok((taker_pays.clone(), taker_gets.clone(), false)),
    };

    // Taker's quality limit in the inverse book = TakerGets / TakerPays.
    let pays_iou = leg_as_quality_iou(&out_leg);
    let gets_iou = leg_as_quality_iou(&in_leg);
    let threshold = rxrpl_amount::get_rate(&gets_iou, &pays_iou).unwrap_or(0);
    if threshold == 0 {
        return Ok((taker_pays.clone(), taker_gets.clone(), false));
    }

    let mut remaining_out = out_leg.clone();
    let mut remaining_in = in_leg.clone();
    let mut crossed = false;
    let book_prefix = &inverse_book.as_bytes()[0..24];

    // The book base for traversal has the quality (low 64 bits) zeroed;
    // `keylet::book_dir` leaves those as hash bytes, so start the walk there.
    let mut probe = book_dir_with_quality(inverse_book, 0);
    'walk: loop {
        let Some(dir_key) = ctx.view.succ(&probe) else { break };
        if &dir_key.as_bytes()[0..24] != book_prefix {
            break; // left this book
        }
        probe = dir_key;
        let dir_quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
        if dir_quality > threshold {
            break; // worse than the taker will accept
        }
        let Some(dir_bytes) = ctx.view.read(&dir_key) else { continue };
        let Ok(dir) = serde_json::from_slice::<Value>(&dir_bytes) else { continue };
        let offers: Vec<rxrpl_primitives::Hash256> = dir
            .get("Indexes")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().and_then(|s| s.parse().ok()))
                    .collect()
            })
            .unwrap_or_default();

        for offer_key in offers {
            if remaining_out.is_zero() {
                break 'walk;
            }
            let Some(ob) = ctx.view.read(&offer_key) else { continue };
            let Ok(offer) = serde_json::from_slice::<Value>(&ob) else { continue };
            if offer.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                continue;
            }
            let owner_str = offer.get("Account").and_then(|v| v.as_str()).unwrap_or("");
            let Ok(owner) = decode_account_id(owner_str) else { continue };
            if &owner == taker {
                continue; // never cross our own offer (rippled steps over it)
            }
            // offer.out = what the offer gives = taker receives = offer.TakerGets.
            // offer.in  = what the offer wants = taker pays   = offer.TakerPays.
            let Some(offer_out) = Leg::parse(&offer["TakerGets"]) else { continue };
            let Some(offer_in) = Leg::parse(&offer["TakerPays"]) else { continue };
            if !is_offer_funded(ctx, &owner, &offer["TakerGets"]) {
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                continue;
            }

            // Full take when the taker wants the whole offer; otherwise a
            // partial take of just `remaining_out`, priced at the offer's
            // quality (`order_in = order_out * rate`, clamped to the offer).
            let full_take = leg_ge(&remaining_out, &offer_out);
            let (order_out, order_in) = if full_take {
                (offer_out.clone(), offer_in.clone())
            } else {
                let rate = rxrpl_amount::from_rate(dir_quality).unwrap_or(IOUAmount::ZERO);
                let computed = rxrpl_amount::Amount::multiply(
                    &leg_to_amount(&remaining_out),
                    &rxrpl_amount::Amount::Iou(rate),
                    in_leg.is_xrp,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
                let order_in = leg_min(&amount_to_leg(&computed, &offer_in), &offer_in);
                (remaining_out.clone(), order_in)
            };

            // Move funds: taker pays order_in (grossed), owner pays order_out.
            pay_in(ctx, taker, taker_acct, &owner, &order_in)?;
            pay_out(ctx, taker, taker_acct, &owner, &order_out)?;

            if full_take {
                // Consume to zero (records the change in metadata) then delete,
                // as rippled consumes before BookTip drops the offer.
                let mut consumed = offer.clone();
                consumed["TakerGets"] = offer_out.with_amount(&IOUAmount::ZERO, 0);
                consumed["TakerPays"] = offer_in.with_amount(&IOUAmount::ZERO, 0);
                let cb = serde_json::to_vec(&consumed).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(offer_key, cb)
                    .map_err(|_| TransactionResult::TefInternal)?;
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
            } else {
                // Reduce the resting offer in place by the filled amounts.
                let new_gets = leg_sub(&offer_out, &order_out);
                let new_pays = leg_sub(&offer_in, &order_in);
                let mut reduced = offer.clone();
                reduced["TakerGets"] = new_gets.with_amount(&new_gets.iou, new_gets.drops);
                reduced["TakerPays"] = new_pays.with_amount(&new_pays.iou, new_pays.drops);
                let rb = serde_json::to_vec(&reduced).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(offer_key, rb)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }

            remaining_out = leg_sub(&remaining_out, &order_out);
            remaining_in = leg_sub(&remaining_in, &order_in);
            crossed = true;
        }
    }

    Ok((
        remaining_out.with_amount(&remaining_out.iou, remaining_out.drops),
        remaining_in.with_amount(&remaining_in.iou, remaining_in.drops),
        crossed,
    ))
}

/// A leg's magnitude as an IOUAmount for quality math (drops become an integer).
fn leg_as_quality_iou(leg: &Leg) -> IOUAmount {
    if leg.is_xrp {
        IOUAmount::from_decimal_string(&leg.drops.to_string()).unwrap_or(IOUAmount::ZERO)
    } else {
        leg.iou
    }
}

/// A leg as a unified `Amount` (XRP drops or IOU).
fn leg_to_amount(leg: &Leg) -> rxrpl_amount::Amount {
    if leg.is_xrp {
        rxrpl_amount::Amount::Xrp(leg.drops)
    } else {
        rxrpl_amount::Amount::Iou(leg.iou)
    }
}

/// Build a leg carrying `amount`, keeping `template`'s currency/issuer/kind.
fn amount_to_leg(amount: &rxrpl_amount::Amount, template: &Leg) -> Leg {
    let mut out = template.clone();
    match amount {
        rxrpl_amount::Amount::Xrp(d) => out.drops = *d,
        rxrpl_amount::Amount::Iou(v) => out.iou = *v,
    }
    out
}

/// The smaller of two like-typed legs.
fn leg_min(a: &Leg, b: &Leg) -> Leg {
    if leg_ge(a, b) { b.clone() } else { a.clone() }
}

/// `a >= b` for like-typed legs.
fn leg_ge(a: &Leg, b: &Leg) -> bool {
    if a.is_xrp {
        a.drops >= b.drops
    } else {
        a.iou >= b.iou
    }
}

/// `a - b` for like-typed legs (same currency).
fn leg_sub(a: &Leg, b: &Leg) -> Leg {
    let mut out = a.clone();
    if a.is_xrp {
        out.drops = a.drops - b.drops;
    } else {
        out.iou = IOUAmount::sub(&a.iou, &b.iou).unwrap_or(IOUAmount::ZERO);
    }
    out
}

/// Move the taker's input to the offer owner: XRP via balances, IOU via the
/// grossed trust-line debit / net credit.
fn pay_in(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    owner: &AccountId,
    amount: &Leg,
) -> Result<(), TransactionResult> {
    if amount.is_xrp {
        let bal = helpers::get_balance(taker_acct) as i64 - amount.drops;
        if bal < 0 {
            return Err(TransactionResult::TecUnfundedOffer);
        }
        helpers::set_balance(taker_acct, bal as u64);
        return credit_xrp(ctx, owner, amount.drops);
    }
    let rate = transfer_rate(ctx, &amount.issuer);
    let gross = grossed(&amount.iou, &rate);
    credit_line(ctx, taker, &amount.issuer, &amount.currency, &gross.negate())?;
    credit_line(ctx, owner, &amount.issuer, &amount.currency, &amount.iou)
}

/// Move the offer owner's output to the taker: XRP via balances, IOU via the
/// grossed owner debit / net taker credit (the difference is the burned fee).
/// The taker's XRP credit goes through `taker_acct` (the caller's working copy),
/// not the view, so the apply's final account write does not clobber it.
fn pay_out(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    owner: &AccountId,
    amount: &Leg,
) -> Result<(), TransactionResult> {
    if amount.is_xrp {
        credit_xrp(ctx, owner, -amount.drops)?;
        helpers::set_balance(taker_acct, helpers::get_balance(taker_acct) + amount.drops as u64);
        return Ok(());
    }
    let rate = transfer_rate(ctx, &amount.issuer);
    let gross = grossed(&amount.iou, &rate);
    credit_line(ctx, owner, &amount.issuer, &amount.currency, &gross.negate())?;
    credit_line(ctx, taker, &amount.issuer, &amount.currency, &amount.iou)
}

/// `amount * rate` using rippled's non-rounding STAmount multiply.
fn grossed(amount: &IOUAmount, rate: &IOUAmount) -> IOUAmount {
    match rxrpl_amount::Amount::multiply(
        &rxrpl_amount::Amount::Iou(*amount),
        &rxrpl_amount::Amount::Iou(*rate),
        false,
    ) {
        Ok(rxrpl_amount::Amount::Iou(v)) => v,
        _ => *amount,
    }
}

/// Remove an offer: owner dir, its quality book dir, erase the SLE, decrement
/// OwnerCount. `book_dir` is the offer's own `BookDirectory` (the quality
/// directory it lives in), not the book base.
fn reap_offer(
    ctx: &mut ApplyContext<'_>,
    owner: &AccountId,
    offer_key: &rxrpl_primitives::Hash256,
    book_dir: &rxrpl_primitives::Hash256,
) -> Result<(), TransactionResult> {
    crate::owner_dir::remove_from_owner_dir(ctx.view, owner, offer_key)?;
    remove_from_book_dir(ctx.view, book_dir, offer_key)?;
    let _ = ctx.view.erase(offer_key);
    let owner_key = keylet::account(owner);
    if let Some(b) = ctx.view.read(&owner_key) {
        if let Ok(mut acct) = serde_json::from_slice::<Value>(&b) {
            helpers::adjust_owner_count(&mut acct, -1);
            if let Ok(nb) = serde_json::to_vec(&acct) {
                let _ = ctx.view.update(owner_key, nb);
            }
        }
    }
    Ok(())
}

/// Extract a 20-byte currency code and issuer AccountId from a TakerPays /
/// TakerGets value. XRP — represented as a bare drops string — maps to
/// all-zero currency and issuer.
fn currency_and_issuer(amount: &Value) -> ([u8; 20], AccountId) {
    if amount.is_string() {
        return ([0u8; 20], AccountId::from([0u8; 20]));
    }
    let currency_str = amount
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut currency = [0u8; 20];
    if currency_str.len() == 3 {
        let b = currency_str.as_bytes();
        currency[12] = b[0];
        currency[13] = b[1];
        currency[14] = b[2];
    } else if currency_str.len() == 40 {
        if let Ok(decoded) = hex::decode(currency_str) {
            if decoded.len() == 20 {
                currency.copy_from_slice(&decoded);
            }
        }
    }
    let issuer = amount
        .get("issuer")
        .and_then(|v| v.as_str())
        .and_then(|s| decode_account_id(s).ok())
        .unwrap_or_else(|| AccountId::from([0u8; 20]));
    (currency, issuer)
}

/// Permissioned DEX flag on an issuer's AccountRoot.
const LSF_PERMISSIONED_DEX: u32 = 0x0080_0000;

/// Check if an IOU asset's issuer requires permissioned DEX access.
///
/// If the issuer has `lsfPermissionedDEX` set, verifies the trader holds
/// accepted credentials from the issuer's PermissionedDomain. XRP assets
/// are always allowed.
fn check_permissioned_asset(
    ctx: &PreclaimContext<'_>,
    trader_id: &AccountId,
    asset: Option<&Value>,
) -> Result<(), TransactionResult> {
    let asset = match asset {
        Some(v) if v.is_object() => v,
        _ => return Ok(()), // XRP or missing -- no restriction
    };

    // Extract issuer from the IOU object
    let issuer_str = match asset.get("issuer").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Ok(()),
    };

    let issuer_id = decode_account_id(issuer_str).map_err(|_| TransactionResult::TemMalformed)?;
    let issuer_key = keylet::account(&issuer_id);

    let issuer_bytes = match ctx.view.read(&issuer_key) {
        Some(b) => b,
        None => return Ok(()), // Issuer not found -- let other checks handle
    };

    let issuer_obj: Value =
        serde_json::from_slice(&issuer_bytes).map_err(|_| TransactionResult::TemMalformed)?;

    let flags = helpers::get_flags(&issuer_obj);
    if flags & LSF_PERMISSIONED_DEX == 0 {
        return Ok(()); // Issuer does not require permissioned DEX
    }

    // Issuer requires PermissionedDEX -- check if trader has credentials.
    // Look up the issuer's PermissionedDomains (seq 0..9) and verify the
    // trader holds at least one accepted credential type from any domain.
    for domain_seq in 0..10u32 {
        let domain_key = keylet::permissioned_domain(&issuer_id, domain_seq);
        let domain_bytes = match ctx.view.read(&domain_key) {
            Some(b) => b,
            None => break, // No more domains
        };

        let domain: Value =
            serde_json::from_slice(&domain_bytes).map_err(|_| TransactionResult::TemMalformed)?;

        if let Some(accepted) = domain.get("AcceptedCredentials").and_then(|v| v.as_array()) {
            for entry in accepted {
                let cred_issuer_str = entry
                    .get("AcceptedCredential")
                    .and_then(|c| c.get("Issuer"))
                    .and_then(|v| v.as_str());
                let cred_type = entry
                    .get("AcceptedCredential")
                    .and_then(|c| c.get("CredentialType"))
                    .and_then(|v| v.as_str());

                if let (Some(ci_str), Some(ct)) = (cred_issuer_str, cred_type) {
                    if let Ok(ci_id) = decode_account_id(ci_str) {
                        let cred_key = keylet::credential(trader_id, &ci_id, ct.as_bytes());
                        if ctx.view.exists(&cred_key) {
                            return Ok(()); // Trader holds an accepted credential
                        }
                    }
                }
            }
        }
    }

    // No valid credential found in any domain
    Err(TransactionResult::TecNoPermission)
}

/// Test whether `owner` currently holds the `TakerGets` amount in full.
/// XRP: AccountRoot.Balance ≥ amount. IOU: holder-side trust line balance ≥ value.
fn is_offer_funded(ctx: &mut ApplyContext<'_>, owner: &AccountId, gets: &Value) -> bool {
    if let Some(drops_str) = gets.as_str() {
        let needed: u64 = drops_str.parse().unwrap_or(0);
        if needed == 0 {
            return false;
        }
        let key = keylet::account(owner);
        let Some(b) = ctx.view.read(&key) else {
            return false;
        };
        let Ok(acct) = serde_json::from_slice::<Value>(&b) else {
            return false;
        };
        let bal: u64 = acct
            .get("Balance")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        return bal >= needed;
    }
    let cur_str = gets.get("currency").and_then(|v| v.as_str()).unwrap_or("");
    let issuer_str = gets.get("issuer").and_then(|v| v.as_str()).unwrap_or("");
    let value: f64 = gets
        .get("value")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    if value <= 0.0 {
        return false;
    }
    let issuer_id = match decode_account_id(issuer_str) {
        Ok(id) => id,
        Err(_) => return false,
    };
    if &issuer_id == owner {
        // Issuer can always issue its own currency.
        return true;
    }
    let cur_bytes = helpers::currency_to_bytes(cur_str);
    let tl_key = keylet::trust_line(owner, &issuer_id, &cur_bytes);
    let Some(tl_bytes) = ctx.view.read(&tl_key) else {
        return false;
    };
    let Ok(tl) = serde_json::from_slice::<Value>(&tl_bytes) else {
        return false;
    };
    let raw: f64 = tl
        .get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("0")
        .parse()
        .unwrap_or(0.0);
    // Balance is stored from the low account's perspective. The non-issuer
    // holder's balance has the opposite sign when the issuer is the low account.
    let issuer_is_low = issuer_id.as_bytes() < owner.as_bytes();
    let holder_view = if issuer_is_low { -raw } else { raw };
    holder_view >= value
}

/// Remove an offer hash from a book directory (inverse of `add_to_dir`).
fn remove_from_book_dir(
    view: &mut dyn crate::view::apply_view::ApplyView,
    book_root: &rxrpl_primitives::Hash256,
    offer_key: &rxrpl_primitives::Hash256,
) -> Result<(), TransactionResult> {
    let entry_hex = offer_key.to_string();
    let mut page = 0u64;
    loop {
        let page_key = if page == 0 {
            *book_root
        } else {
            keylet::dir_node(book_root, page)
        };
        let bytes = match view.read(&page_key) {
            Some(b) => b,
            None => return Ok(()),
        };
        let mut dir: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
        let next_page = dir.get("IndexNext").and_then(|v| v.as_u64()).unwrap_or(0);
        let removed = if let Some(indexes) = dir.get_mut("Indexes").and_then(|v| v.as_array_mut()) {
            let original = indexes.len();
            indexes.retain(|v| v.as_str() != Some(entry_hex.as_str()));
            indexes.len() != original
        } else {
            false
        };
        if removed {
            let empty = dir
                .get("Indexes")
                .and_then(|v| v.as_array())
                .map(|a| a.is_empty())
                .unwrap_or(true);
            if empty && page == 0 {
                let _ = view.erase(&page_key);
            } else {
                let new_bytes =
                    serde_json::to_vec(&dir).map_err(|_| TransactionResult::TefInternal)?;
                let _ = view.update(page_key, new_bytes);
            }
            return Ok(());
        }
        if next_page == 0 {
            return Ok(());
        }
        page = next_page;
    }
}
