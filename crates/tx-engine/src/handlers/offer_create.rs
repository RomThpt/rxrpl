use rxrpl_amendment::feature::feature_id;
use rxrpl_amount::{IOUAmount, from_rate, offer_quality, offer_quality_round_even, round_quality};
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
pub(crate) fn book_dir_with_quality(book_base: &Hash256, quality: u64) -> Hash256 {
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
///
/// `number_switchover` selects rippled's `STAmount::divide` canonicalisation:
/// once `fixUniversalNumber` is active the quotient is reduced round-half-to-
/// even (`offer_quality_round_even`); before it, by truncation (`offer_quality`).
/// The book directory of a modern offer (e.g. `04F5F33…` landing on `…7EA4`)
/// only matches rippled under the even reduction.
fn offer_book_quality(taker_pays: &Value, taker_gets: &Value, number_switchover: bool) -> u64 {
    match (amount_to_iou(taker_pays), amount_to_iou(taker_gets)) {
        (Some(p), Some(g)) => {
            let q = if number_switchover {
                offer_quality_round_even(&p, &g)
            } else {
                offer_quality(&p, &g)
            };
            q.unwrap_or(0)
        }
        _ => 0,
    }
}

/// OfferCreate transaction flags.
const TF_PASSIVE: u64 = 0x0001_0000;
const TF_IMMEDIATE_OR_CANCEL: u64 = 0x0002_0000;
const TF_FILL_OR_KILL: u64 = 0x0004_0000;
/// tfSell flag: the offer is a sell, so TakerGets is the exact amount.
const TF_SELL: u64 = 0x0008_0000;

/// Offer ledger-entry flags. rippled translates the transaction `tfPassive` /
/// `tfSell` flags into these (note `lsfSell` differs from `tfSell`); no other
/// transaction flag is persisted on the Offer SLE.
const LSF_PASSIVE: u64 = 0x0001_0000;
const LSF_SELL: u64 = 0x0002_0000;

/// rippled's `Quality::kMaxTickSize` — no rounding at or above 16 digits.
const MAX_TICK_SIZE: u8 = 16;

/// Round a non-negative decimal magnitude to the nearest integer drop, ties to
/// even — rippled's `Number`-based `STAmount(XRP)` canonicalisation
/// (`Number::operator rep()`, "round towards nearest, and on tie towards even"),
/// not truncation.
fn round_drops_half_even(dec: &str) -> Option<u64> {
    let dec = dec.strip_prefix('-').unwrap_or(dec);
    let (int_str, frac_str) = dec.split_once('.').unwrap_or((dec, ""));
    let mut drops: u64 = int_str.parse().ok()?;
    if let Some(&first) = frac_str.as_bytes().first() {
        let first = first - b'0';
        let rest_nonzero = frac_str.as_bytes()[1..].iter().any(|&b| b != b'0');
        if first > 5 || (first == 5 && (rest_nonzero || drops & 1 == 1)) {
            drops = drops.checked_add(1)?;
        }
    }
    Some(drops)
}

/// Reserialize an IOU offer side with a recomputed magnitude, preserving its
/// currency/issuer. Returns `None` for XRP sides (no tick rounding applies to
/// a recomputed native amount here).
fn rebuild_iou(original: &Value, value: &IOUAmount) -> Option<Value> {
    // An XRP side is a bare drops string, not an IOU object: the tick
    // re-derivation yields a fractional drops magnitude which rippled rounds to
    // the nearest drop (round half to even, `STAmount(XRP)` via `Number`), not
    // truncates. Without this the re-derivation silently failed on XRP offers
    // and the tick snap was skipped.
    if original.is_string() {
        let drops = round_drops_half_even(&value.to_decimal_string())?;
        return Some(Value::String(drops.to_string()));
    }
    let obj = original.as_object()?;
    let mut out = obj.clone();
    out.insert("value".into(), Value::from(value.to_decimal_string()));
    Some(Value::Object(out))
}

/// Apply issuer `TickSize` rounding to the offer amounts before crossing,
/// matching rippled's OfferCreate. When an IOU side's issuer sets a TickSize
/// below the 16-digit maximum, the offer quality is rounded up to that many
/// significant digits and the non-fixed side re-derived, snapping the offer to
/// the issuer's price grid. Offers with no tick size are returned unchanged.
fn tick_round_amounts(
    ctx: &ApplyContext<'_>,
    taker_pays: &Value,
    taker_gets: &Value,
    is_sell: bool,
) -> (Value, Value) {
    let tick_of = |amount: &Value| -> u8 {
        let Some(issuer) = amount.get("issuer").and_then(|v| v.as_str()) else {
            return MAX_TICK_SIZE;
        };
        let Ok(id) = decode_account_id(issuer) else {
            return MAX_TICK_SIZE;
        };
        ctx.view
            .read(&keylet::account(&id))
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|a| a.get("TickSize").and_then(|v| v.as_u64()))
            .map(|t| t as u8)
            .unwrap_or(MAX_TICK_SIZE)
    };

    let tick = tick_of(taker_pays).min(tick_of(taker_gets));
    if tick >= MAX_TICK_SIZE {
        return (taker_pays.clone(), taker_gets.clone());
    }

    let (Some(p_iou), Some(g_iou)) = (amount_to_iou(taker_pays), amount_to_iou(taker_gets)) else {
        return (taker_pays.clone(), taker_gets.clone());
    };
    // The tick grid snaps the offer's quality, so it must be computed with the
    // same (amendment-gated) divide canonicalisation as the placed book rate.
    let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
    let quality = if number_switchover {
        offer_quality_round_even(&p_iou, &g_iou)
    } else {
        offer_quality(&p_iou, &g_iou)
    };
    let Ok(quality) = quality else {
        return (taker_pays.clone(), taker_gets.clone());
    };
    let Ok(rate) = from_rate(round_quality(quality, tick)) else {
        return (taker_pays.clone(), taker_gets.clone());
    };

    if is_sell {
        if let Some(new_pays) = IOUAmount::multiply(&g_iou, &rate)
            .ok()
            .and_then(|p| rebuild_iou(taker_pays, &p))
        {
            return (new_pays, taker_gets.clone());
        }
    } else if let Some(new_gets) = IOUAmount::divide(&p_iou, &rate)
        .ok()
        .and_then(|g| rebuild_iou(taker_gets, &g))
    {
        return (taker_pays.clone(), new_gets);
    }
    (taker_pays.clone(), taker_gets.clone())
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

        // PermissionedDEX gates only domain-scoped offers (those carrying a
        // DomainID); an open offer trades on the public book unrestricted. The
        // credential check applies only when a DomainID is present.
        if ctx.rules.enabled(&feature_id("PermissionedDEX")) && ctx.tx.get("DomainID").is_some() {
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

        // The offer's Sequence (and its keylet) is rippled's `getSeqProxy()`
        // value: the TicketSequence when the transaction spends a ticket,
        // otherwise the transaction's Sequence. It must come from the TX, not the
        // account Sequence — the engine already consumed the sender's
        // Sequence/Ticket centrally, so the account Sequence is one ahead.
        let sequence = helpers::tx_seq_proxy_value(ctx.tx);

        let (pays_currency, pays_issuer) = currency_and_issuer(&ctx.tx["TakerPays"]);
        let (gets_currency, gets_issuer) = currency_and_issuer(&ctx.tx["TakerGets"]);

        // Snap the offer to the issuer's TickSize grid before crossing, exactly
        // as rippled does: the placed rate and re-derived amounts both flow from
        // the rounded quality. No-op when neither issuer sets a tick size.
        let is_sell = ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & TF_SELL != 0;
        let (taker_pays, taker_gets) =
            tick_round_amounts(ctx, &ctx.tx["TakerPays"], &ctx.tx["TakerGets"], is_sell);

        // The sender's Sequence/Ticket and fee are consumed centrally by the
        // engine (parent sandbox) before doApply, so they are charged even when
        // this transaction ends in a tec claim below.

        // Unfunded check (rippled preclaim): an offer must be at least partially
        // funded in the asset it sells (TakerGets), else tecUNFUNDED_OFFER — fee
        // and sequence charged, no offer placed, no crossing. accountFunds: an
        // IOU side is the holder's trust-line balance (the issuer of its own
        // currency is always funded); an XRP side is the liquid balance above the
        // owner reserve (`xrpLiquid`).
        let owner_count = acct.get("OwnerCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let funded = match Leg::parse(&taker_gets) {
            Some(g) if g.is_xrp => {
                // preclaim runs before the fee is taken; add it back to the
                // post-fee balance the parent sandbox left us.
                let pre_fee = helpers::get_balance(&acct).saturating_add(helpers::get_fee(ctx.tx));
                pre_fee > ctx.fees.account_reserve(owner_count)
            }
            Some(g) => !owner_funds_leg(ctx, &account_id, &g).is_zero(),
            None => true,
        };
        if !funded {
            let nb = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .update(acct_key, nb)
                .map_err(|_| TransactionResult::TemMalformed)?;
            return Ok(TransactionResult::TecUnfundedOffer);
        }

        // OfferSequence (cancel-and-replace): rippled deletes the account's own
        // offer at that sequence before crossing — removed from its book and
        // owner directories with the owner reserve released. It is not an error
        // if the offer is already gone. The OwnerCount decrement lands on `acct`
        // (written back below), not the view copy a generic reap would clobber.
        if let Some(cancel_seq) = ctx.tx.get("OfferSequence").and_then(|v| v.as_u64()) {
            let cancel_key = keylet::offer(&account_id, cancel_seq as u32);
            if let Some(old) = ctx
                .view
                .read(&cancel_key)
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .filter(|o| o.get("LedgerEntryType").and_then(|v| v.as_str()) == Some("Offer"))
            {
                if let Some(book_dir) = old
                    .get("BookDirectory")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<Hash256>().ok())
                {
                    remove_from_book_dir(ctx.view, &book_dir, &cancel_key)?;
                }
                crate::owner_dir::remove_from_owner_dir(ctx.view, &account_id, &cancel_key)?;
                let _ = ctx.view.erase(&cancel_key);
                helpers::adjust_owner_count(&mut acct, -1);
            }
        }

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
            &taker_pays,
            &taker_gets,
            &inverse_book,
            is_sell,
        )?;

        // Nothing left to place when either side is exhausted (fully crossed):
        // rippled places no resting offer. Commit the taker's mutations.
        let commit_acct =
            |ctx: &mut ApplyContext<'_>, acct: &Value| -> Result<(), TransactionResult> {
                let nb = serde_json::to_vec(acct).map_err(|_| TransactionResult::TemMalformed)?;
                ctx.view
                    .update(acct_key, nb)
                    .map_err(|_| TransactionResult::TemMalformed)?;
                Ok(())
            };
        if value_is_zero(&remaining_pays) || value_is_zero(&remaining_gets) {
            commit_acct(ctx, &acct)?;
            return Ok(TransactionResult::TesSuccess);
        }

        // rippled clears the leftover offer when crossing exhausted the taker's
        // funds in the asset it sells: an offer it cannot fund at all is not
        // placed (OfferCreate.cpp:481, `takerInBalance <= 0`). Applied to an IOU
        // gets side, whose post-crossing balance is already committed to the
        // view; an XRP gets side is reserve-gated below instead.
        if crossed {
            if let Some(gets_leg) = Leg::parse(&taker_gets) {
                if !gets_leg.is_xrp && owner_funds_leg(ctx, &account_id, &gets_leg).is_zero() {
                    commit_acct(ctx, &acct)?;
                    return Ok(TransactionResult::TesSuccess);
                }
            }
        }

        // A remainder survives crossing. Time-in-force flags decide its fate
        // before any resting offer is placed.
        let tx_flags = ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0);
        if tx_flags & TF_FILL_OR_KILL != 0 {
            // FillOrKill: failure to fully cross kills the whole operation.
            commit_acct(ctx, &acct)?;
            return Ok(TransactionResult::TecKilled);
        }
        if tx_flags & TF_IMMEDIATE_OR_CANCEL != 0 {
            // ImmediateOrCancel: the remainder is cancelled, not placed. An offer
            // that transferred nothing returns tecKILLED; otherwise tesSUCCESS.
            commit_acct(ctx, &acct)?;
            return Ok(if crossed {
                TransactionResult::TesSuccess
            } else {
                TransactionResult::TecKilled
            });
        }

        // Owner reserve: a resting offer needs reserve for one more owned
        // object. rippled returns tecINSUF_RESERVE_OFFER — fee and sequence
        // charged, no offer placed — when the account cannot afford it AND
        // nothing crossed (a crossing offer may still place below reserve). Read
        // the count fresh: a cancelled OfferSequence has already decremented it.
        let reserve_count = acct.get("OwnerCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if !crossed && helpers::get_balance(&acct) < ctx.fees.account_reserve(reserve_count + 1) {
            // A claimed tec discards the doApply changes and keeps only the fee
            // and sequence — return Err so the engine rolls the child back (and,
            // under tapRETRY, defers the whole transaction to a later pass to be
            // reclaimed there, matching mainnet's TransactionIndex and balances).
            return Err(TransactionResult::TecInsufReserveOffer);
        }

        let offer_key = keylet::offer(&account_id, sequence);

        // The order-book directory is keyed by the book base (currencies +
        // issuers) with its low 64 bits replaced by the offer's quality (rate),
        // so offers sort by price. rippled stores this as the offer's
        // BookDirectory and tags the directory with the rate + book assets.
        // The book directory quality is the rate of the offer AS PLACED — the
        // leftover amounts after crossing (remaining_pays/remaining_gets), which
        // are what the Offer SLE carries — not the original tx amounts. A partial
        // cross that trims TakerGets but leaves TakerPays shifts the rate, and
        // rippled keys the directory on the placed offer's rate.
        let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
        let quality = offer_book_quality(&remaining_pays, &remaining_gets, number_switchover);
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

        // Build the Offer SLE. sfFlags, sfBookNode and sfOwnerNode are REQUIRED
        // in rippled's Offer ledger format, so they always serialize (even at
        // zero); only sfExpiration is optional.
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
        // An offer carries its own Expiration (a hard ledger-time deadline) when
        // the transaction sets one; rippled copies it onto the Offer SLE.
        if let Some(exp) = ctx.tx.get("Expiration").and_then(|v| v.as_u64()) {
            offer.insert("Expiration".into(), Value::from(exp));
        }
        // sfFlags, sfBookNode and sfOwnerNode are REQUIRED fields on the Offer
        // SLE — rippled writes all three unconditionally, so they serialize even
        // when zero (a root-page offer carrying no flags). Only the
        // transaction's tfPassive/tfSell map onto the SLE, and tfSell (0x80000)
        // becomes lsfSell (0x20000) — they are not the same bit.
        let offer_flags = (if tx_flags & TF_PASSIVE != 0 {
            LSF_PASSIVE
        } else {
            0
        }) | (if tx_flags & TF_SELL != 0 { LSF_SELL } else { 0 });
        offer.insert("Flags".into(), Value::from(offer_flags));
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
        if self.is_xrp {
            self.drops == 0
        } else {
            self.iou.is_zero()
        }
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
    round: bool,
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
    let balance =
        IOUAmount::from_decimal_string(cur).map_err(|_| TransactionResult::TefInternal)?;
    let holder_is_high = holder.as_bytes() > issuer.as_bytes();
    let delta = if holder_is_high { gain.negate() } else { *gain };
    // Legacy crossing (pre-Number 2013 ledgers) truncates the smaller term;
    // modern Flow conversions round to nearest. See [`IOUAmount::add_round`].
    let new = if round {
        IOUAmount::add_round(&balance, &delta)
    } else {
        IOUAmount::add(&balance, &delta)
    }
    .map_err(|_| TransactionResult::TefInternal)?;
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
    is_sell: bool,
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
    // The threshold must be computed with the SAME divide canonicalisation
    // used to store the offers' BookDirectory qualities (`offer_book_quality`):
    // once `fixUniversalNumber` is active rippled's getRate rounds half-to-even,
    // otherwise it truncates. Using the truncating `get_rate` here while the
    // dir qualities are round-even left the threshold 1 ULP low, so the walk
    // broke before an exactly-equal-priced resting offer and crossed nothing.
    let pays_iou = leg_as_quality_iou(&out_leg);
    let gets_iou = leg_as_quality_iou(&in_leg);
    let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
    let threshold = if number_switchover {
        rxrpl_amount::get_rate_round_even(&gets_iou, &pays_iou)
    } else {
        rxrpl_amount::get_rate(&gets_iou, &pays_iou)
    }
    .unwrap_or(0);
    if threshold == 0 {
        return Ok((taker_pays.clone(), taker_gets.clone(), false));
    }

    let mut remaining_out = out_leg.clone();
    let mut remaining_in = in_leg.clone();
    let mut crossed = false;
    let book_prefix = &inverse_book.as_bytes()[0..24];
    let fok = ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & TF_FILL_OR_KILL != 0;

    // Autobridge: when NEITHER leg is XRP, the offer also competes an
    // XRP-intermediate path (`in -> XRP -> out`) against the direct book. Prebuild
    // that two-hop strand once; `try_bridge_step` crosses it per band by quality.
    let bridge = if !in_leg.is_xrp && !out_leg.is_xrp {
        build_flow_strand(
            ctx,
            &[
                taker_gets.clone(),
                Value::String("0".to_string()),
                taker_pays.clone(),
            ],
        )
    } else {
        None
    };

    // The book base for traversal has the quality (low 64 bits) zeroed;
    // `keylet::book_dir` leaves those as hash bytes, so start the walk there.
    let mut probe = book_dir_with_quality(inverse_book, 0);
    'walk: while let Some(dir_key) = ctx.view.succ(&probe) {
        if &dir_key.as_bytes()[0..24] != book_prefix {
            break; // left this book
        }
        probe = dir_key;
        let dir_quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
        if dir_quality > threshold {
            break; // worse than the taker will accept
        }
        // Interleave the AMM at this quality band BEFORE the resting CLOB offers.
        // rippled crosses whichever of the synthetic AMM offer / CLOB tip is
        // better at each quality level (BookStep tryAMM/execOffer). `amm_hop`'s
        // fee-adjusted spot gate admits the AMM only when it strictly beats
        // `dir_quality`, so this fires exactly when the AMM is at least as good
        // as the offers about to cross at this band; it is a no-op otherwise.
        if !remaining_out.is_zero() && !remaining_in.is_zero() {
            if let Some(b) = &bridge {
                try_bridge_step(
                    ctx,
                    taker,
                    taker_acct,
                    b,
                    &in_leg,
                    &out_leg,
                    &mut remaining_out,
                    &mut remaining_in,
                    &mut crossed,
                    dir_quality,
                )?;
                if remaining_out.is_zero() || remaining_in.is_zero() {
                    break 'walk;
                }
            }
            try_amm_step(
                ctx,
                taker,
                taker_acct,
                &in_leg,
                &mut remaining_out,
                &mut remaining_in,
                &mut crossed,
                dir_quality,
                fok,
            )?;
            if remaining_out.is_zero() || remaining_in.is_zero() {
                break 'walk; // AMM alone met the demand (or exhausted the taker's funds)
            }
        }
        let Some(dir_bytes) = ctx.view.read(&dir_key) else {
            continue;
        };
        let Ok(dir) = serde_json::from_slice::<Value>(&dir_bytes) else {
            continue;
        };
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
            // A tfSell offer sells its whole TakerGets and accepts any TakerPays
            // (rippled: deliver = kMaxNative), so the walk is bounded by the
            // taker's remaining input, not its TakerPays demand.
            let done = if is_sell {
                remaining_in.is_zero()
            } else {
                remaining_out.is_zero()
            };
            if done {
                break 'walk;
            }
            let Some(ob) = ctx.view.read(&offer_key) else {
                continue;
            };
            let Ok(offer) = serde_json::from_slice::<Value>(&ob) else {
                continue;
            };
            if offer.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                continue;
            }
            let owner_str = offer.get("Account").and_then(|v| v.as_str()).unwrap_or("");
            let Ok(owner) = decode_account_id(owner_str) else {
                continue;
            };
            if &owner == taker {
                continue; // never cross our own offer (rippled steps over it)
            }
            // offer.out = what the offer gives = taker receives = offer.TakerGets.
            // offer.in  = what the offer wants = taker pays   = offer.TakerPays.
            let Some(offer_out) = Leg::parse(&offer["TakerGets"]) else {
                continue;
            };
            let Some(offer_in) = Leg::parse(&offer["TakerPays"]) else {
                continue;
            };
            // An expired offer is removed 0-fill during the book walk, before any
            // funds/quality crossing (rippled `OfferStream`: `sfExpiration <=
            // parentCloseTime` via `hasExpired`). Core behaviour, not amendment
            // gated, so it applies on every ledger.
            if let Some(exp) = offer.get("Expiration").and_then(|v| v.as_u64()) {
                if exp <= ctx.view.parent_close_time() as u64 {
                    reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                    continue;
                }
            }
            // A bad offer with a zero amount on either side is removed 0-fill
            // (rippled `OfferStream`: `amount.empty()`).
            if offer_out.is_zero() || offer_in.is_zero() {
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                continue;
            }
            // A maker whose IN asset (what it receives) is deep-frozen can neither
            // receive nor send it, so the offer is removed 0-fill (rippled
            // `OfferStream`: `isDeepFrozen(owner, assetIn)`).
            if is_deep_frozen(ctx, &owner, &offer_in) {
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                continue;
            }
            // Owner-funds clamp: an offer can give at most what its owner
            // holds. Fully funded → the whole offer is available; underfunded
            // but positive → fill against the funded amount; zero → reap.
            let funds = owner_funds_leg(ctx, &owner, &offer_out);
            if funds.is_zero() {
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                continue;
            }
            let avail_out = leg_min(&offer_out, &funds);
            // A tfSell take is bounded by what the taker's remaining input can buy
            // at this offer's quality (it sells its whole TakerGets); a buy take is
            // bounded by the taker's remaining TakerPays demand.
            let take_out = if is_sell {
                let rate = rxrpl_amount::from_rate(dir_quality).unwrap_or(IOUAmount::ZERO);
                leg_min(&avail_out, &out_for_in(&remaining_in, &rate, &offer_out))
            } else {
                leg_min(&remaining_out, &avail_out)
            };

            // Full take when the taker consumes the whole original offer;
            // otherwise a partial take of `take_out`, priced at the offer's
            // quality (`order_in = take_out * rate`, clamped to the offer).
            let full_take = leg_ge(&take_out, &offer_out);
            let (order_out, order_in) = if full_take {
                (offer_out.clone(), offer_in.clone())
            } else if is_sell && !leg_ge(&take_out, &avail_out) {
                // tfSell input-bound: `take_out` was derived from the taker's whole
                // remaining input at this quality, so the taker gives exactly that
                // input — not a re-multiplied approximation that would leave a dust
                // remainder unsold and diverge the maker's residual TakerPays.
                (take_out.clone(), remaining_in.clone())
            } else {
                // Demand/funds-limited partial fill: the taker pays the CEIL price
                // for the delivered output, never more than the resting offer.
                // Price from the offer's own amounts (`in = ceil(offer_in * out /
                // offer_out)`), not the book's quantized quality rate — on an
                // underfunded offer the quantized rate ceils one drop high, which
                // cascades through the taker's remaining budget.
                let rate = rxrpl_amount::from_rate(dir_quality).unwrap_or(IOUAmount::ZERO);
                let order_in = leg_min(
                    &in_for_out_offer(&offer_in, &take_out, &offer_out, &rate),
                    &offer_in,
                );
                (take_out.clone(), order_in)
            };

            // Move funds: taker pays order_in (grossed), owner pays order_out.
            // Legacy Taker semantics: `order_out` is the NET the taker receives;
            // the owner's debit is grossed up by the output issuer's transfer
            // fee inside `pay_out`.
            // Post-fixUniversalNumber, rippled's Flow rounds trust-line balance
            // updates to nearest (Number); the legacy pre-2013 path truncated the
            // smaller term, which loses precision when a large balance is credited
            // a small amount (e.g. a 10^10 balance minus ~3e3 diverges 3 ULP).
            pay_in(
                ctx,
                taker,
                taker_acct,
                &owner,
                &order_in,
                number_switchover,
                false,
            )?;
            pay_out(
                ctx,
                taker,
                taker_acct,
                &owner,
                taker,
                &order_out,
                number_switchover,
            )?;

            // The offer's available liquidity is exhausted when the take
            // consumes all of `avail_out` — either the whole offer (`full_take`)
            // or, when the owner is underfunded, all of the owner's funds.
            // rippled deletes such an offer (the owner can no longer fund the
            // remainder) rather than leaving an unfunded husk in the book.
            let exhausted = leg_ge(&take_out, &avail_out);
            if exhausted {
                // Record the metadata delta (zero for a full take, the unfunded
                // remainder for a depleted owner) then delete, as rippled
                // consumes before BookTip / offerDelete drops the offer.
                let mut consumed = offer.clone();
                if full_take {
                    consumed["TakerGets"] = offer_out.with_amount(&IOUAmount::ZERO, 0);
                    consumed["TakerPays"] = offer_in.with_amount(&IOUAmount::ZERO, 0);
                } else {
                    let new_gets = leg_sub(&offer_out, &order_out);
                    let new_pays = leg_sub_round(&offer_in, &order_in);
                    consumed["TakerGets"] = new_gets.with_amount(&new_gets.iou, new_gets.drops);
                    consumed["TakerPays"] = new_pays.with_amount(&new_pays.iou, new_pays.drops);
                }
                let cb =
                    serde_json::to_vec(&consumed).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(offer_key, cb)
                    .map_err(|_| TransactionResult::TefInternal)?;
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
            } else {
                // Reduce the resting offer in place by the filled amounts.
                let new_gets = leg_sub(&offer_out, &order_out);
                let new_pays = leg_sub_round(&offer_in, &order_in);
                let mut reduced = offer.clone();
                reduced["TakerGets"] = new_gets.with_amount(&new_gets.iou, new_gets.drops);
                reduced["TakerPays"] = new_pays.with_amount(&new_pays.iou, new_pays.drops);
                let rb =
                    serde_json::to_vec(&reduced).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(offer_key, rb)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }

            // A tfSell fill can deliver more than the remaining TakerPays demand;
            // clamp the demand at zero rather than underflowing the drops/mantissa.
            remaining_out = if is_sell && leg_ge(&order_out, &remaining_out) {
                leg_sub(&remaining_out, &remaining_out)
            } else {
                leg_sub(&remaining_out, &order_out)
            };
            remaining_in = leg_sub(&remaining_in, &order_in);
            crossed = true;
        }
    }

    // Tail: the AMM may still beat every remaining CLOB up to the taker's overall
    // limit — the book was empty, was exhausted, or the walk stopped at a dir
    // worse than `threshold` while the AMM's spot still beats it. Cross the AMM up
    // to `threshold` (the loosest gate). rippled interleaves the AMM by quality
    // rather than gating it on whether any CLOB crossed, so this is NOT `!crossed`
    // gated: a single crossed CLOB offer must not suppress better AMM liquidity.
    if !remaining_out.is_zero() && !remaining_in.is_zero() {
        if let Some(b) = &bridge {
            try_bridge_step(
                ctx,
                taker,
                taker_acct,
                b,
                &in_leg,
                &out_leg,
                &mut remaining_out,
                &mut remaining_in,
                &mut crossed,
                threshold,
            )?;
        }
        try_amm_step(
            ctx,
            taker,
            taker_acct,
            &in_leg,
            &mut remaining_out,
            &mut remaining_in,
            &mut crossed,
            threshold,
            fok,
        )?;
    }

    Ok((
        remaining_out.with_amount(&remaining_out.iou, remaining_out.drops),
        remaining_in.with_amount(&remaining_in.iou, remaining_in.drops),
        crossed,
    ))
}

/// Cross the book's AMM pool (if present and strictly better than `cq`) up to the
/// target quality `cq`, limited to the taker's funds, updating the remaining
/// demand/budget and `crossed` in place. Mirrors rippled's per-quality-band AMM
/// injection (`BookStep::tryAMM`): `amm_hop`'s fee-adjusted spot gate admits the
/// AMM only when it strictly beats `cq`, so this is a no-op unless the AMM is at
/// least as good as the CLOB at this band.
#[allow(clippy::too_many_arguments)]
fn try_amm_step(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    in_leg: &Leg,
    remaining_out: &mut Leg,
    remaining_in: &mut Leg,
    crossed: &mut bool,
    cq: u64,
    fok: bool,
) -> Result<(), TransactionResult> {
    // rippled's flowCross limits the crossing input to the taker's funds
    // (`sendMax = min(takerAmount.in, accountFunds)`, OfferCreate.cpp:400): the
    // AMM swap would otherwise overdraw the taker (its sold-asset balance goes
    // negative) when TakerGets exceeds what it holds.
    let taker_funds = owner_funds_leg(ctx, taker, in_leg);
    let budget = leg_min(remaining_in, &taker_funds);
    if budget.is_zero() {
        return Ok(());
    }
    // partial = !fok: a fill-or-kill offer crosses all-or-nothing, so an AMM that
    // cannot deliver the full demand within budget delivers zero. A per-band spend
    // by a FoK offer that later fails to fully cross is rolled back atomically —
    // the transactor returns tecKILLED and the engine discards all mutations.
    if let Some((delivered, spent)) = amm_hop(
        ctx,
        taker,
        taker_acct,
        taker,
        remaining_out,
        &budget,
        /*skip_input_debit=*/ false,
        /*skip_output_credit=*/ false,
        /*create_missing_dest_line=*/ true,
        /*partial=*/ !fok,
        /*target_quality=*/ Some(cq),
    )? {
        *remaining_out = leg_sub(remaining_out, &delivered);
        *remaining_in = leg_sub(remaining_in, &spent);
        *crossed = true;
    }
    Ok(())
}

/// Cross the XRP-intermediate autobridge path (`in -> XRP -> out`, a prebuilt
/// two-hop `bridge` strand) as a competing source in the `cross_offers` walk,
/// mirroring [`try_amm_step`]: cross bridge passes whose realised quality still
/// beats `cq` (the current CLOB band, or the taker's overall limit at the tail),
/// updating the remaining demand/budget in place. Each `execute_strand_pass` is
/// checkpointed and rolled back when its quality is worse than `cq`, so the
/// bridge only wins the bands where it is genuinely better — matching rippled's
/// per-band interleave of the direct book and the autobridge path.
#[allow(clippy::too_many_arguments)]
fn try_bridge_step(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    bridge: &FlowStrand,
    in_leg: &Leg,
    out_leg: &Leg,
    remaining_out: &mut Leg,
    remaining_in: &mut Leg,
    crossed: &mut bool,
    cq: u64,
) -> Result<(), TransactionResult> {
    let taker_funds = owner_funds_leg(ctx, taker, in_leg);
    let mut budget = leg_min(remaining_in, &taker_funds);
    // Persist the AMM fib counter across passes (rippled `AMMContext`): each
    // committed pass advances `amm_iters`, growing the synthetic AMM offer
    // geometrically so the strand converges in a handful of passes. A fresh
    // context per pass would reseed the smallest fib chunk every time, delivering
    // an identical dust sliver and looping ~10^5 times to drain the budget.
    let mut amm_ctx = AmmContext::new(false);
    loop {
        if remaining_out.is_zero() || budget.is_zero() {
            break;
        }
        let cp = ctx.view.checkpoint();
        amm_ctx.clear();
        let ro = leg_to_number(remaining_out);
        let ri = leg_to_number(&budget);
        match execute_strand_pass(
            ctx,
            taker,
            taker_acct,
            taker,
            bridge,
            &ri,
            &ro,
            &mut amm_ctx,
        ) {
            Some(res) if !res.out_amt.is_zero() && !res.in_amt.is_zero() => {
                let in_q = num_quality_iou(&res.in_amt, in_leg.is_xrp);
                let out_q = num_quality_iou(&res.out_amt, out_leg.is_xrp);
                let eff_q = rxrpl_amount::get_rate(&in_q, &out_q).unwrap_or(u64::MAX);
                if !rxrpl_amount::is_better_quality(eff_q, cq) {
                    ctx.view.rollback(cp);
                    break;
                }
                let d = leg_from_magnitude(&res.out_amt.to_iou(), out_leg);
                let s = leg_from_magnitude(&res.in_amt.to_iou(), in_leg);
                *remaining_out = leg_sub(remaining_out, &d);
                *remaining_in = leg_sub(remaining_in, &s);
                budget = leg_sub(&budget, &s);
                *crossed = true;
                amm_ctx.update();
            }
            _ => {
                ctx.view.rollback(cp);
                break;
            }
        }
    }
    Ok(())
}

/// Output deliverable for a given input at a resting offer's price. This mirrors
/// rippled's `Quality::ceilInStrict` (the input-limited path in `BookStep`):
/// `out = in / q` rounded DOWN (`divRoundStrict`), where `q` is the offer's
/// book-directory quality rate (`rate = in/out`) — the fixed quality bucket the
/// offer lives in, NOT the offer's drifted post-partial-fill `TakerPays/TakerGets`
/// ratio. Pricing from the bucketed dir rate is what makes the consumed/owner
/// amounts byte-exact (e.g. mainnet SOLO fills land on `…2747`, not `…2750`).
/// Magnitudes are pure `IOUAmount` (drops count as integers) to avoid the
/// native/IOU normalisation hazard of mixed-asset multiply.
fn out_for_in(in_amt: &Leg, rate: &IOUAmount, offer_out: &Leg) -> Leg {
    let in_iou = leg_as_quality_iou(in_amt);
    // Price the fill off the book directory's quality (`rate = in/out`, the fixed
    // quality bucket the offer lives in), exactly as rippled's `BookStep` uses
    // `offer.quality().rate()` rather than the offer's current (post-partial-fill)
    // `TakerPays/TakerGets` ratio. Re-deriving the rate from the drifted current
    // amounts left interior book-hop outputs 1-2 ULP off `divRoundStrict`.
    let out_iou =
        IOUAmount::div_round(&in_iou, rate, /*round_up*/ false).unwrap_or(IOUAmount::ZERO);
    leg_from_magnitude(&out_iou, offer_out)
}

/// Build a leg in `template`'s asset carrying magnitude `mag`. For XRP the
/// magnitude is floored to an integer drop count (rippled never delivers a
/// fractional drop); for an IOU it is the value directly.
fn leg_from_magnitude(mag: &IOUAmount, template: &Leg) -> Leg {
    let mut out = template.clone();
    if template.is_xrp {
        let s = mag.to_decimal_string();
        let whole = s.split('.').next().unwrap_or("0");
        // Saturate rather than collapse to 0 on overflow: a magnitude beyond
        // i64::MAX drops only arises from the unbounded-budget sentinel converted
        // to XRP (an interior hop's `out_for_in(unbounded_in, rate)` in the
        // reverse pass). It means "no budget cap", so it must read as huge —
        // `unwrap_or(0)` made the budget bind to zero and starved the hop. No real
        // XRP amount exceeds i64::MAX (max supply ~1e17 drops), so fills unchanged.
        out.drops = whole.parse::<i64>().unwrap_or(i64::MAX);
    } else {
        out.iou = *mag;
    }
    out
}

/// Input required for a given output at a book directory's quality rate,
/// rounded up (`in = out * rate`, the taker pays at least the price).
fn in_for_out(out_amt: &Leg, rate: &IOUAmount, in_template: &Leg) -> Leg {
    let amt = rxrpl_amount::Amount::mul_round(
        &leg_to_amount(out_amt),
        &rxrpl_amount::Amount::Iou(*rate),
        in_template.is_xrp,
        true,
    )
    .unwrap_or(leg_to_amount(in_template));
    amount_to_leg(&amt, in_template)
}

/// Input required for a demand-limited partial take of a resting offer, priced
/// from the offer's own amounts: `in = ceil(offer_in * out / offer_out)`. For an
/// XRP input this multiplies before dividing so no drop is lost — rippled's Flow
/// prices partial takes proportionally on the offer's amounts, whereas the book
/// directory's quantized quality rate can under-price by a drop. IOU input keeps
/// the rate-based path (16-digit mantissa, unaffected by the drop truncation).
fn in_for_out_offer(offer_in: &Leg, out_amt: &Leg, offer_out: &Leg, rate: &IOUAmount) -> Leg {
    if !offer_in.is_xrp {
        return in_for_out(out_amt, rate, offer_in);
    }
    use rxrpl_amount::number::{Number, RoundModeGuard, RoundingMode};
    let priced = Number::from_int(offer_in.drops)
        .mul(&Number::from_iou(&leg_as_quality_iou(out_amt)))
        .div(&Number::from_iou(&leg_as_quality_iou(offer_out)));
    let _g = RoundModeGuard::new(RoundingMode::Upward);
    let mut out = offer_in.clone();
    out.drops = priced.to_xrp_drops_mode() as i64;
    out
}

/// Cross-currency Payment book crossing under the single-taker conversion model
/// (the taker both pays the input and receives the output — used for
/// `Account == Destination` currency conversions). Walks the book of offers
/// giving `target_out`'s asset for `budget_in`'s asset, best price first, with
/// NO per-offer quality cap: rippled fills greedily until the target output is
/// delivered or the `SendMax` input budget is spent. Unfunded offers are reaped;
/// funded ones are fully or partially filled. The taker's XRP balance moves
/// through `taker_acct` (the caller writes it back). Returns
/// `(delivered_out, spent_in)`.
pub(crate) fn cross_book_payment(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    target_out: &Value,
    budget_in: &Value,
) -> Result<(Value, Value), TransactionResult> {
    let demand_out = Leg::parse(target_out).ok_or(TransactionResult::TemBadAmount)?;
    let in_tmpl = Leg::parse(budget_in).ok_or(TransactionResult::TemBadAmount)?;

    // The source cannot spend more of an IOU than it holds: cap the SendMax
    // budget at the taker's available balance in the input asset (rippled's
    // source-funds limit). XRP input is already guarded by `pay_in`.
    let mut budget = in_tmpl.clone();
    if !in_tmpl.is_xrp {
        let funds = owner_funds_leg(ctx, taker, &in_tmpl);
        if leg_ge(&budget, &funds) {
            budget = funds;
        }
    }

    let (delivered, spent) = cross_book_hop(
        ctx,
        taker,
        taker_acct,
        dest,
        &demand_out,
        &budget,
        /*skip_input_debit=*/ false,
        /*skip_output_credit=*/ false,
        /*single_band=*/ false,
    )?;
    Ok((
        delivered.with_amount(&delivered.iou, delivered.drops),
        spent.with_amount(&spent.iou, spent.drops),
    ))
}

/// Cross a SINGLE order book, delivering up to `demand_out` (its magnitude is
/// the demand cap) while spending up to `budget_in` (its magnitude is the
/// budget). Best price first via `succ()` over the book's quality
/// sub-directories. Returns `(delivered_out, spent_in)` as `Leg`s in the
/// demand/budget assets. This is the byte-exact `BookStep` primitive shared by
/// the single-book conversion ([`cross_book_payment`]) and the multi-hop
/// path-payment chain ([`cross_path_payment`]).
///
/// `skip_input_debit` / `skip_output_credit` exist for INTERMEDIATE hops of a
/// multi-hop strand, where the input came from the previous book's offer owners
/// (so the taker is not debited again) and the output feeds the next book's
/// offer owners (so no account is credited the delivery). For those hops the
/// counter-party leg of the move is realised by the adjacent hop touching the
/// SAME issuer's trust lines, keeping the intermediate issuer's obligations
/// balanced. The first hop debits the taker; the last hop credits `dest`.
#[allow(clippy::too_many_arguments)]
fn cross_book_hop(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    demand_out: &Leg,
    budget_in: &Leg,
    skip_input_debit: bool,
    skip_output_credit: bool,
    single_band: bool,
) -> Result<(Leg, Leg), TransactionResult> {
    let inverse_book = keylet::book_dir(
        &budget_in.currency,
        &budget_in.issuer,
        &demand_out.currency,
        &demand_out.issuer,
    );

    let mut remaining_out = demand_out.clone();
    let mut remaining_in = budget_in.clone();
    // When `single_band`, the walk consumes only ONE quality level (the first
    // page on which a funded offer is actually filled). rippled's `forEachOffer`
    // processes one quality band per `BookStep::rev/fwd`; the multi-path Flow loop
    // (`flow_multi`) drives one band per pass so a hop's CLOB liquidity interleaves
    // with the shared AMM's fib chunks. Unfunded/skipped offers do not lock the
    // band (they are reaped and the walk continues to the next page).
    let mut band_quality: Option<u64> = None;
    // Accumulate the realised delivery / spend directly from each fill. Computing
    // them as `start - remaining` underflows for an interior hop, whose demand cap
    // is the huge `unbounded_leg` sentinel (~1e96): a real ~0.2 delivery is below
    // its ULP, so the subtraction would round to 0 and starve the next hop.
    let mut delivered = leg_from_magnitude(&IOUAmount::ZERO, demand_out);
    delivered.drops = 0;
    let mut spent = leg_from_magnitude(&IOUAmount::ZERO, budget_in);
    spent.drops = 0;
    let book_prefix = inverse_book.as_bytes()[0..24].to_vec();
    let mut probe = book_dir_with_quality(&inverse_book, 0);
    'walk: while let Some(dir_key) = ctx.view.succ(&probe) {
        if dir_key.as_bytes()[0..24] != book_prefix[..] {
            break;
        }
        probe = dir_key;
        let dir_quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
        // Bounded to a single quality band: once an offer at quality Q has been
        // filled, stop at the first page of a worse quality.
        if single_band {
            if let Some(bq) = band_quality {
                if bq != dir_quality {
                    break 'walk;
                }
            }
        }
        // Interleave the AMM at this quality band BEFORE crossing the resting
        // CLOB offers, mirroring cross_offers' try_amm_step (rippled BookStep
        // tryAMM/execOffer): amm_hop's fee-adjusted spot gate admits the pool
        // only when it strictly beats this band's quality, so it crosses exactly
        // where the AMM is at least as good as the offers about to fill and is a
        // no-op on a pure-CLOB book. The output lands on `dest`; interior hops
        // carry the previous hop's delivery so their input is not funds-capped.
        if !remaining_out.is_zero() && !remaining_in.is_zero() {
            let amm_budget = if skip_input_debit {
                remaining_in.clone()
            } else {
                leg_min(&remaining_in, &owner_funds_leg(ctx, taker, budget_in))
            };
            if !amm_budget.is_zero() {
                if let Some((amm_out, amm_spent)) = amm_hop(
                    ctx,
                    taker,
                    taker_acct,
                    dest,
                    &remaining_out,
                    &amm_budget,
                    skip_input_debit,
                    skip_output_credit,
                    /*create_missing_dest_line=*/ true,
                    /*partial=*/ true,
                    /*target_quality=*/ Some(dir_quality),
                )? {
                    remaining_out = leg_sub_round(&remaining_out, &amm_out);
                    remaining_in = leg_sub_round(&remaining_in, &amm_spent);
                    delivered = leg_add(&delivered, &amm_out);
                    spent = leg_add(&spent, &amm_spent);
                }
            }
            if remaining_out.is_zero() || remaining_in.is_zero() {
                break 'walk;
            }
        }
        let rate = rxrpl_amount::from_rate(dir_quality).unwrap_or(IOUAmount::ZERO);
        let Some(dir_bytes) = ctx.view.read(&dir_key) else {
            continue;
        };
        let Ok(dir) = serde_json::from_slice::<Value>(&dir_bytes) else {
            continue;
        };
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
            if remaining_out.is_zero() || remaining_in.is_zero() {
                break 'walk;
            }
            let Some(ob) = ctx.view.read(&offer_key) else {
                continue;
            };
            let Ok(offer) = serde_json::from_slice::<Value>(&ob) else {
                continue;
            };
            if offer.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                continue;
            }
            let owner_str = offer.get("Account").and_then(|v| v.as_str()).unwrap_or("");
            let Ok(owner) = decode_account_id(owner_str) else {
                continue;
            };
            if &owner == taker {
                continue;
            }
            let Some(offer_out) = Leg::parse(&offer["TakerGets"]) else {
                continue;
            };
            let Some(offer_in) = Leg::parse(&offer["TakerPays"]) else {
                continue;
            };
            let funds = owner_funds_leg(ctx, &owner, &offer_out);
            if funds.is_zero() {
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                continue;
            }
            let avail_out = leg_min(&offer_out, &funds);

            // Pricing rate: the offer's book-directory quality (`in/out`). A
            // degenerate page quality of zero only arises in synthetic ledgers
            // whose offer sits at the book base rather than a quality sub-dir;
            // there, fall back to the offer's current amounts so those fixtures
            // still price. Real mainnet book pages always carry a valid quality.
            let eff_rate = if rate.is_zero() {
                IOUAmount::divide(
                    &leg_as_quality_iou(&offer_in),
                    &leg_as_quality_iou(&offer_out),
                )
                .unwrap_or(IOUAmount::ZERO)
            } else {
                rate
            };

            // Output capped by remaining demand, funded availability, and what
            // the remaining input budget can buy at this offer's price.
            let budget_out = out_for_in(&remaining_in, &eff_rate, &offer_out);
            let mut take_out = leg_min(&remaining_out, &avail_out);
            let budget_binds = leg_ge(&take_out, &budget_out);
            if budget_binds {
                take_out = budget_out;
            }

            let full_take = leg_ge(&take_out, &offer_out);
            let funds_limited = !full_take && !budget_binds && leg_ge(&take_out, &avail_out);
            let (order_out, order_in) = if full_take {
                (offer_out.clone(), offer_in.clone())
            } else if budget_binds {
                // Input-limited: spend the whole remaining budget, deliver floor.
                (take_out.clone(), remaining_in.clone())
            } else if funds_limited {
                // Funds-limited (owner partially unfunded): rippled scales the
                // resting offer — TakerPays * funded_TakerGets / TakerGets, rounded
                // DOWN — rather than re-pricing the funded output at the book's
                // bucket rate (which ceils and over-charges by up to 1 drop). The
                // over-charge cascades through an interior bridge hop; a
                // demand-limited take (below) still pays the ceil price.
                let ratio = IOUAmount::divide(
                    &leg_as_quality_iou(&avail_out),
                    &leg_as_quality_iou(&offer_out),
                )
                .unwrap_or(IOUAmount::ZERO);
                let scaled = IOUAmount::multiply(&leg_as_quality_iou(&offer_in), &ratio)
                    .unwrap_or(IOUAmount::ZERO);
                let order_in = leg_min(
                    &leg_min(&leg_from_magnitude(&scaled, &offer_in), &offer_in),
                    &remaining_in,
                );
                (take_out.clone(), order_in)
            } else {
                // Demand-limited: pay the ceil price for the delivered output,
                // never exceeding the resting offer or the remaining budget.
                let priced = in_for_out_offer(&offer_in, &take_out, &offer_out, &eff_rate);
                let order_in = leg_min(&leg_min(&priced, &offer_in), &remaining_in);
                (take_out.clone(), order_in)
            };

            pay_in(
                ctx,
                taker,
                taker_acct,
                &owner,
                &order_in,
                true,
                skip_input_debit,
            )?;
            // `order_out` is the GROSS the offer/owner gives (the TakerGets it
            // reduces by); `net_out` is what `dest` actually receives after the
            // output issuer's transfer fee. The book demand (`remaining_out`) is
            // tracked in NET terms, so it decrements by `net_out`, not the gross.
            let net_out = pay_out_gross(
                ctx,
                taker,
                taker_acct,
                &owner,
                dest,
                &order_out,
                true,
                skip_output_credit,
            )?;

            if full_take {
                let mut consumed = offer.clone();
                consumed["TakerGets"] = offer_out.with_amount(&IOUAmount::ZERO, 0);
                consumed["TakerPays"] = offer_in.with_amount(&IOUAmount::ZERO, 0);
                let cb =
                    serde_json::to_vec(&consumed).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(offer_key, cb)
                    .map_err(|_| TransactionResult::TefInternal)?;
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
            } else {
                let new_gets = leg_sub_round(&offer_out, &order_out);
                let new_pays = leg_sub_round(&offer_in, &order_in);
                let mut reduced = offer.clone();
                reduced["TakerGets"] = new_gets.with_amount(&new_gets.iou, new_gets.drops);
                reduced["TakerPays"] = new_pays.with_amount(&new_pays.iou, new_pays.drops);
                let rb =
                    serde_json::to_vec(&reduced).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(offer_key, rb)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }

            remaining_out = leg_sub_round(&remaining_out, &net_out);
            remaining_in = leg_sub_round(&remaining_in, &order_in);
            delivered = leg_add(&delivered, &net_out);
            spent = leg_add(&spent, &order_in);
            // Lock the band on the first funded fill: subsequent pages of a worse
            // quality are not consumed this call.
            if single_band {
                band_quality = Some(dir_quality);
            }
        }
    }

    Ok((delivered, spent))
}

/// Cross a chain of order books (a single resolved path), forwarding each
/// book's output as the next book's input budget. `boundaries[0]` is the
/// source asset (carrying the `SendMax`/budget magnitude on entry), each
/// interior boundary is an intermediate currency, and `boundaries[last]` is the
/// delivered asset (the `Amount`). Returns `(delivered_out, spent_in)` in the
/// last/first boundary assets respectively.
///
/// This is the input-limited forward flow of rippled's `StrandFlow`: the source
/// pushes its budget through hop 0, the realised output funds hop 1, and so on.
/// It is byte-exact for partial payments whose `SendMax` is the binding limit
/// (the validated repro shape). Each interior hop must consume the entirety of
/// the previous hop's delivery — otherwise the intermediate issuer's
/// obligations would not balance — which is enforced by returning
/// `TecPathPartial` (discarding the sandbox) when a downstream book cannot
/// absorb the upstream delivery.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cross_path_payment(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    boundaries: &[Value],
    target_out: &Value,
    budget_in: &Value,
) -> Result<(Value, Value), TransactionResult> {
    let n = boundaries.len();
    if n < 2 {
        return Err(TransactionResult::TecPathDry);
    }

    // First hop budget = SendMax, capped at the source's funds for an IOU leg.
    let mut carry = Leg::parse(budget_in).ok_or(TransactionResult::TemBadAmount)?;
    if !carry.is_xrp {
        let funds = owner_funds_leg(ctx, taker, &carry);
        if leg_ge(&carry, &funds) {
            carry = funds;
        }
    }
    let final_demand = Leg::parse(target_out).ok_or(TransactionResult::TemBadAmount)?;

    // rippled `StrandFlow` output-limited case: for a pure-AMM multi-hop strand
    // whose required input fits the budget, reverse-price from the demanded
    // output back to the source input and apply the exact (reverse-rounded)
    // per-hop amounts, delivering the full `target_out`. The forward-only flow
    // below cannot: it pushes the whole budget through hop 0 (unbounded interior
    // demand), over-produces the intermediate asset, and fails the interior
    // consume invariant. Returns `Ok(None)` (no mutation) for input-limited or
    // CLOB-bearing strands, which fall through to the forward flow.
    if let Some(res) = reverse_amm_strand(
        ctx,
        taker,
        taker_acct,
        dest,
        boundaries,
        &final_demand,
        &carry,
    )? {
        return Ok(res);
    }

    let mut delivered = final_demand.clone();
    let mut source_spent = carry.clone();
    for hop in 0..(n - 1) {
        let in_tmpl = Leg::parse(&boundaries[hop]).ok_or(TransactionResult::TemBadAmount)?;
        let out_tmpl = Leg::parse(&boundaries[hop + 1]).ok_or(TransactionResult::TemBadAmount)?;
        let is_first = hop == 0;
        let is_last = hop == n - 2;

        // Carry the realised magnitude into this hop's budget, keeping the
        // boundary's currency/issuer/kind.
        let mut budget = in_tmpl.clone();
        if budget.is_xrp {
            budget.drops = carry.drops;
        } else {
            budget.iou = carry.iou;
        }

        // Demand: the final hop is capped by the requested Amount; interior hops
        // deliver as much as the budget can buy (unbounded demand).
        let demand = if is_last {
            let mut d = out_tmpl.clone();
            if d.is_xrp {
                d.drops = final_demand.drops;
            } else {
                d.iou = final_demand.iou;
            }
            d
        } else {
            unbounded_leg(&out_tmpl)
        };

        let (clob_out, clob_spent) = cross_book_hop(
            ctx, taker, taker_acct, dest, &demand, &budget, /*skip_input_debit=*/ !is_first,
            /*skip_output_credit=*/ !is_last, /*single_band=*/ false,
        )?;

        // Residual liquidity from the AMM pool for this pair, on the budget not
        // yet spent on the order book and the demand not yet met. For a
        // pure-AMM hop the book delivers nothing and the swap handles the whole
        // budget. (Strict quality interleaving of coexisting AMM + CLOB is not
        // yet modelled — pure-AMM and pure-CLOB hops are exact.)
        let mut got = clob_out.clone();
        let mut spent = clob_spent.clone();
        let residual_budget = leg_sub(&budget, &clob_spent);
        let residual_demand = leg_sub(&demand, &clob_out);
        if !residual_budget.is_zero() && !residual_demand.is_zero() {
            if let Some((amm_out, amm_spent)) = amm_hop(
                ctx,
                taker,
                taker_acct,
                dest,
                &residual_demand,
                &residual_budget,
                /*skip_input_debit=*/ !is_first,
                /*skip_output_credit=*/ !is_last,
                /*create_missing_dest_line=*/ false,
                /*partial=*/ true,
                /*target_quality=*/ None,
            )? {
                got = leg_add(&got, &amm_out);
                spent = leg_add(&spent, &amm_spent);
            }
        }

        // An interior hop must fully consume what the previous hop delivered, or
        // the intermediate currency does not balance across the two books.
        if !is_first && !legs_eq(&spent, &carry) {
            return Err(TransactionResult::TecPathPartial);
        }
        if is_first {
            source_spent = spent;
        }
        // rippled charges the intermediate issuer's transfer fee once as the IOU
        // ripples through the interior account between two consecutive books. Net
        // the carry (the next hop's input budget) by that rate on an interior IOU
        // boundary whose issuer is a third party. The final delivery, XRP
        // boundaries, issuer-party hops, and fee-free issuers carry no fee, so the
        // guard leaves net == gross exactly as before for those.
        carry = got.clone();
        if !is_last && !out_tmpl.is_xrp && out_tmpl.issuer != *taker && out_tmpl.issuer != *dest {
            let rate = transfer_rate(ctx, &out_tmpl.issuer);
            let one = IOUAmount::from_parts(1_000_000_000, -9, false).unwrap();
            if rate != one {
                carry.iou = IOUAmount::div_round(&carry.iou, &rate, false).unwrap_or(carry.iou);
            }
        }
        delivered = got;
    }

    Ok((
        delivered.with_amount(&delivered.iou, delivered.drops),
        source_spent.with_amount(&source_spent.iou, source_spent.drops),
    ))
}

/// Equality of two like-typed legs (exact drops, exact IOU magnitude).
fn legs_eq(a: &Leg, b: &Leg) -> bool {
    if a.is_xrp {
        a.drops == b.drops
    } else {
        a.iou == b.iou
    }
}

/// A `Leg` in `template`'s asset carrying an effectively unbounded magnitude,
/// used as the demand cap for an interior hop (the budget is the real limit).
fn unbounded_leg(template: &Leg) -> Leg {
    let mut out = template.clone();
    if template.is_xrp {
        out.drops = i64::MAX;
    } else {
        // A budget far above any real book size, but well below the STAmount max
        // exponent (+80): the reverse pass prices `budget / rate` in `out_for_in`,
        // and a near-max budget divided by a sub-1 rate overflows `div_round` to
        // ZERO — read as a zero budget, which starved an interior IOU->XRP hop.
        // ~1e46 is orders of magnitude above any real amount (< ~1e20) yet leaves
        // ample headroom for the divide.
        out.iou =
            IOUAmount::from_parts(9_999_999_999_999_999, 30, false).unwrap_or(IOUAmount::ZERO);
    }
    out
}

/// A `Leg`'s magnitude as a `Number` (drops as an integer; IOU as its value).
fn leg_to_number(leg: &Leg) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    if leg.is_xrp {
        Number::from_int(leg.drops)
    } else {
        Number::from_iou(&leg.iou)
    }
}

/// Build a `Leg` carrying `mag`, keeping `template`'s currency/issuer/kind. XRP
/// magnitudes floor to an integer drop count.
fn number_to_leg(mag: &rxrpl_amount::number::Number, template: &Leg) -> Leg {
    let mut out = template.clone();
    if template.is_xrp {
        out.drops = mag.to_xrp_drops() as i64;
    } else {
        out.iou = mag.to_iou();
    }
    out
}

/// `a <= b` for two `Number`s.
fn num_le(a: &rxrpl_amount::number::Number, b: &rxrpl_amount::number::Number) -> bool {
    let d = a.sub(b);
    d.is_zero() || d.negative()
}

/// `a > b` for two `Number`s.
fn num_gt(a: &rxrpl_amount::number::Number, b: &rxrpl_amount::number::Number) -> bool {
    let d = a.sub(b);
    !d.is_zero() && !d.negative()
}

/// The AMM pool's holding of one asset as a `Number`: the pseudo-account's XRP
/// `Balance` for native, or its issuer trust-line balance for an IOU.
fn pool_balance_number(
    ctx: &ApplyContext<'_>,
    pool: &AccountId,
    is_xrp: bool,
    cur: &[u8; 20],
    iss: &AccountId,
) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    if is_xrp {
        let key = keylet::account(pool);
        let bal = ctx
            .view
            .read(&key)
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|a| helpers::get_balance(&a))
            .unwrap_or(0);
        Number::from_int(bal as i64)
    } else {
        crate::amm_helpers::iou_holding_number(ctx.view, pool, iss, cur)
    }
}

/// Swap one hop through an AMM pool for the `(budget_in -> demand_out)` pair,
/// delivering up to `demand_out` while spending up to `budget_in`. Returns
/// `Some((delivered, spent))` or `None` when no AMM SLE exists for the pair (the
/// caller then treats the hop as order-book-only).
///
/// Mirrors [`super::payment`]'s `try_amm_conversion` (byte-exact `swapAssetIn` /
/// `swapAssetOut` on the live pool reserves), with the multi-hop skip flags: an
/// interior input is not debited from the taker (the previous pool's out-holding
/// already fell by the same amount), and an interior output is not credited to
/// `dest` (it becomes the next pool's input).
#[allow(clippy::too_many_arguments)]
/// Auto-create `holder`'s trust line to `issuer` for `currency` when an AMM swap
/// delivers an IOU the holder has never held (rippled `rippleCredit` ->
/// `trustCreate`). The receiver bears the reserve, so its side carries the
/// reserve flag and (unless it has lsfDefaultRipple) the NoRipple flag; the owner
/// count is bumped on the in-memory `holder_acct` (committed by the caller). The
/// balance is left at zero here — the caller's `set_iou_holding` writes it with
/// the correct low/high sign.
fn create_iou_trust_line(
    ctx: &mut ApplyContext<'_>,
    holder: &AccountId,
    holder_acct: &mut Value,
    issuer: &AccountId,
    currency: &[u8; 20],
) -> Result<(), TransactionResult> {
    const LSF_LOW_RESERVE: u64 = 0x0001_0000;
    const LSF_HIGH_RESERVE: u64 = 0x0002_0000;
    const LSF_LOW_NO_RIPPLE: u64 = 0x0010_0000;
    const LSF_HIGH_NO_RIPPLE: u64 = 0x0020_0000;
    const LSF_DEFAULT_RIPPLE: u32 = 0x0080_0000;
    const NO_ACCOUNT: &str = "rrrrrrrrrrrrrrrrrrrrBZbvji";

    let tl_key = keylet::trust_line(holder, issuer, currency);
    let holder_is_low = holder.as_bytes() < issuer.as_bytes();
    let cur_str = currency_code_str(currency);
    let holder_str = encode_account_id(holder);
    let issuer_str = encode_account_id(issuer);

    let holder_lacks_default_ripple = ctx
        .view
        .read(&keylet::account(holder))
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .map(|a| helpers::get_flags(&a) & LSF_DEFAULT_RIPPLE == 0)
        .unwrap_or(true);

    let mut flags: u64 = if holder_is_low {
        LSF_LOW_RESERVE
    } else {
        LSF_HIGH_RESERVE
    };
    if holder_lacks_default_ripple {
        flags |= if holder_is_low {
            LSF_LOW_NO_RIPPLE
        } else {
            LSF_HIGH_NO_RIPPLE
        };
    }

    let holder_limit =
        serde_json::json!({ "currency": cur_str, "issuer": holder_str, "value": "0" });
    let issuer_limit =
        serde_json::json!({ "currency": cur_str, "issuer": issuer_str, "value": "0" });
    let (low_limit, high_limit) = if holder_is_low {
        (holder_limit, issuer_limit)
    } else {
        (issuer_limit, holder_limit)
    };

    let holder_node = add_to_owner_dir(ctx.view, holder, &tl_key)?;
    let issuer_node = add_to_owner_dir(ctx.view, issuer, &tl_key)?;
    let (low_node, high_node) = if holder_is_low {
        (holder_node, issuer_node)
    } else {
        (issuer_node, holder_node)
    };

    let tl = serde_json::json!({
        "LedgerEntryType": "RippleState",
        "Balance": { "currency": cur_str, "issuer": NO_ACCOUNT, "value": "0" },
        "LowLimit": low_limit,
        "HighLimit": high_limit,
        "LowNode": format!("{low_node:016X}"),
        "HighNode": format!("{high_node:016X}"),
        "Flags": flags,
        // Placeholder so the central PreviousTxnID stamping records this tx on
        // the newly created line (it only touches entries already exposing it).
        "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
        "PreviousTxnLgrSeq": 0u32,
    });
    let bytes = serde_json::to_vec(&tl).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .insert(tl_key, bytes)
        .map_err(|_| TransactionResult::TefInternal)?;

    helpers::adjust_owner_count(holder_acct, 1);

    // The line entered the issuer's owner directory; re-touch its account root so
    // the central PreviousTxnID stamping records it (rippled re-writes it).
    let issuer_key = keylet::account(issuer);
    if let Some(issuer_bytes) = ctx.view.read(&issuer_key) {
        let _ = ctx.view.update(issuer_key, issuer_bytes);
    }
    Ok(())
}

/// A pool-balance `Number` as the `IOUAmount` used for `get_rate`/quality: XRP
/// uses its drops integer as the IOU value, an IOU its own value. Mirrors
/// `flow.rs::quality_iou`.
fn num_quality_iou(n: &rxrpl_amount::number::Number, is_xrp: bool) -> IOUAmount {
    if is_xrp {
        IOUAmount::from_decimal_string(&n.to_xrp_drops().to_string()).unwrap_or(IOUAmount::ZERO)
    } else {
        n.to_iou()
    }
}

#[allow(clippy::too_many_arguments)]
fn amm_hop(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    demand_out: &Leg,
    budget_in: &Leg,
    skip_input_debit: bool,
    skip_output_credit: bool,
    create_missing_dest_line: bool,
    partial: bool,
    target_quality: Option<u64>,
) -> Result<Option<(Leg, Leg)>, TransactionResult> {
    let in_xrp = budget_in.is_xrp;
    let out_xrp = demand_out.is_xrp;

    let amm_key = keylet::amm(
        &budget_in.currency,
        &budget_in.issuer,
        &demand_out.currency,
        &demand_out.issuer,
    );
    let Some(amm_bytes) = ctx.view.read(&amm_key) else {
        return Ok(None);
    };
    let Ok(amm): Result<Value, _> = serde_json::from_slice(&amm_bytes) else {
        return Ok(None);
    };
    let Some(pool_str) = amm.get("Account").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let Ok(pool_id) = decode_account_id(pool_str) else {
        return Ok(None);
    };
    let tfee = amm.get("TradingFee").and_then(|v| v.as_u64()).unwrap_or(0) as u16;

    let pool_in = pool_balance_number(
        ctx,
        &pool_id,
        in_xrp,
        &budget_in.currency,
        &budget_in.issuer,
    );
    let pool_out = pool_balance_number(
        ctx,
        &pool_id,
        out_xrp,
        &demand_out.currency,
        &demand_out.issuer,
    );
    if pool_in.is_zero() || pool_out.is_zero() {
        return Ok(None);
    }

    let budget_num = leg_to_number(budget_in);
    let demand_num = leg_to_number(demand_out);
    if budget_num.is_zero() || budget_num.negative() {
        return Ok(None);
    }

    // Spot-price-quality gate (rippled AMMLiquidity::getOffer, AMMLiquidity.cpp:
    // 184-190): an offer crossing may consume the AMM only when its spot quality
    // STRICTLY beats the taker's limit quality and is not within 1e-7 of it —
    // otherwise deliver nothing so cross_offers rests the full offer. The spot
    // must be FEE-ADJUSTED: the taker pays the trading fee on the input, so the
    // effective in/out is spot / (1 - fee) (worse than the fee-free spot). An AMM
    // whose raw spot beats the limit but whose fee-adjusted price does not must
    // not cross. Payments pass None (any quality is acceptable,
    // BookPaymentStep::checkQualityThreshold == true).
    if let Some(cq) = target_quality {
        let one_minus_fee = IOUAmount::divide(
            &IOUAmount::from_decimal_string(&(100_000u32 - u32::from(tfee)).to_string())
                .unwrap_or(IOUAmount::ZERO),
            &IOUAmount::from_decimal_string("100000").unwrap_or(IOUAmount::ZERO),
        )
        .unwrap_or(IOUAmount::ZERO);
        let out_q = num_quality_iou(&pool_out, out_xrp);
        let out_adj = IOUAmount::multiply(&out_q, &one_minus_fee).unwrap_or(out_q);
        let spq = rxrpl_amount::get_rate(&num_quality_iou(&pool_in, in_xrp), &out_adj).ok();
        let Some(spq) = spq else {
            return Ok(None);
        };
        if !rxrpl_amount::is_better_quality(spq, cq)
            || rxrpl_amount::within_relative_distance(spq, cq)
        {
            return Ok(None);
        }
    }

    // Output-limited (deliver the demand) when the input required fits the
    // budget; otherwise input-limited (spend the whole budget). A non-partial
    // (fill-or-kill) crossing is all-or-nothing: it delivers the FULL demand or
    // nothing (never a partial spend), matching rippled's flow() with
    // partialPayment=false.
    let out_full =
        crate::amm_helpers::swap_asset_in(&pool_in, &pool_out, &budget_num, tfee, out_xrp);
    let (spent_num, deliver_num) = if !partial {
        match crate::amm_helpers::swap_asset_out(&pool_in, &pool_out, &demand_num, tfee, in_xrp) {
            Some(needed) if num_le(&needed, &budget_num) => (needed, demand_num),
            _ => return Ok(None),
        }
    } else if num_le(&out_full, &demand_num) {
        (budget_num, out_full)
    } else {
        match crate::amm_helpers::swap_asset_out(&pool_in, &pool_out, &demand_num, tfee, in_xrp) {
            Some(needed) if num_le(&needed, &budget_num) => (needed, demand_num),
            _ => (budget_num, out_full),
        }
    };
    if deliver_num.is_zero() || spent_num.is_zero() {
        return Ok(None);
    }

    // --- Apply the input: move `spent` from the source into the pool. ---
    if in_xrp {
        let drops = spent_num.to_xrp_drops() as i64;
        if !skip_input_debit {
            let bal = helpers::get_balance(taker_acct) as i64 - drops;
            if bal < 0 {
                return Err(TransactionResult::TecUnfundedPayment);
            }
            helpers::set_balance(taker_acct, bal as u64);
        }
        credit_xrp(ctx, &pool_id, drops)?;
    } else {
        if !skip_input_debit {
            let funds = crate::amm_helpers::iou_holding_number(
                ctx.view,
                taker,
                &budget_in.issuer,
                &budget_in.currency,
            );
            crate::amm_helpers::set_iou_holding(
                ctx.view,
                taker,
                &budget_in.issuer,
                &budget_in.currency,
                &funds.sub(&spent_num),
            )?;
        }
        let pool_h = crate::amm_helpers::iou_holding_number(
            ctx.view,
            &pool_id,
            &budget_in.issuer,
            &budget_in.currency,
        );
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &pool_id,
            &budget_in.issuer,
            &budget_in.currency,
            &pool_h.add(&spent_num),
        )?;
    }

    // --- Apply the output: move `delivered` from the pool to the recipient. ---
    if out_xrp {
        let drops = deliver_num.to_xrp_drops() as i64;
        credit_xrp(ctx, &pool_id, -drops)?;
        if !skip_output_credit {
            if dest == taker {
                helpers::set_balance(taker_acct, helpers::get_balance(taker_acct) + drops as u64);
            } else {
                credit_xrp(ctx, dest, drops)?;
            }
        }
    } else {
        let pool_h = crate::amm_helpers::iou_holding_number(
            ctx.view,
            &pool_id,
            &demand_out.issuer,
            &demand_out.currency,
        );
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &pool_id,
            &demand_out.issuer,
            &demand_out.currency,
            &pool_h.sub(&deliver_num),
        )?;
        if !skip_output_credit {
            // rippled's rippleCredit auto-creates the receiver's trust line the
            // first time an AMM swap delivers an IOU it has never held.
            if create_missing_dest_line {
                let tl_key = keylet::trust_line(dest, &demand_out.issuer, &demand_out.currency);
                if ctx.view.read(&tl_key).is_none() {
                    create_iou_trust_line(
                        ctx,
                        dest,
                        taker_acct,
                        &demand_out.issuer,
                        &demand_out.currency,
                    )?;
                }
            }
            let dest_h = crate::amm_helpers::iou_holding_number(
                ctx.view,
                dest,
                &demand_out.issuer,
                &demand_out.currency,
            );
            crate::amm_helpers::set_iou_holding(
                ctx.view,
                dest,
                &demand_out.issuer,
                &demand_out.currency,
                &dest_h.add(&deliver_num),
            )?;
        }
    }

    Ok(Some((
        number_to_leg(&deliver_num, demand_out),
        number_to_leg(&spent_num, budget_in),
    )))
}

/// Per-hop AMM pool snapshot for the reverse-then-forward strand flow.
struct AmmHop {
    pool_id: AccountId,
    in_xrp: bool,
    out_xrp: bool,
    in_cur: [u8; 20],
    in_iss: AccountId,
    out_cur: [u8; 20],
    out_iss: AccountId,
    tfee: u16,
    pool_in: rxrpl_amount::number::Number,
    pool_out: rxrpl_amount::number::Number,
}

/// Whether the order book identified by `inverse_book` holds at least one
/// resting offer directory page — i.e. the hop carries CLOB liquidity and is not
/// a pure-AMM hop. (succ from quality 0 returns the best-price page; a key that
/// shares the 24-byte book prefix means a page exists.)
fn book_has_resting_offer(ctx: &ApplyContext<'_>, inverse_book: &Hash256) -> bool {
    let probe = book_dir_with_quality(inverse_book, 0);
    match ctx.view.succ(&probe) {
        Some(dir_key) => dir_key.as_bytes()[0..24] == inverse_book.as_bytes()[0..24],
        None => false,
    }
}

/// Move a single AMM hop's exact reverse-computed `(in, out)`: the input flows
/// from the source (or, for an interior hop with `skip_input_debit`, simply
/// lands in this pool — the previous pool already shed the shared intermediate
/// asset) into the pool; the output flows from the pool to `dest` (or, for an
/// interior hop with `skip_output_credit`, stays as the next pool's input).
/// Mirrors the application half of [`amm_hop`] with caller-supplied amounts.
#[allow(clippy::too_many_arguments)]
fn apply_amm_move(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    hop: &AmmHop,
    in_num: &rxrpl_amount::number::Number,
    out_num: &rxrpl_amount::number::Number,
    skip_input_debit: bool,
    skip_output_credit: bool,
) -> Result<(), TransactionResult> {
    // Input: source / previous-pool -> this pool.
    if hop.in_xrp {
        let drops = in_num.to_xrp_drops() as i64;
        if !skip_input_debit {
            let bal = helpers::get_balance(taker_acct) as i64 - drops;
            if bal < 0 {
                return Err(TransactionResult::TecUnfundedPayment);
            }
            helpers::set_balance(taker_acct, bal as u64);
        }
        credit_xrp(ctx, &hop.pool_id, drops)?;
    } else {
        if !skip_input_debit {
            let funds =
                crate::amm_helpers::iou_holding_number(ctx.view, taker, &hop.in_iss, &hop.in_cur);
            crate::amm_helpers::set_iou_holding(
                ctx.view,
                taker,
                &hop.in_iss,
                &hop.in_cur,
                &funds.sub(in_num),
            )?;
        }
        let pool_h = crate::amm_helpers::iou_holding_number(
            ctx.view,
            &hop.pool_id,
            &hop.in_iss,
            &hop.in_cur,
        );
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &hop.pool_id,
            &hop.in_iss,
            &hop.in_cur,
            &pool_h.add(in_num),
        )?;
    }
    // Output: this pool -> dest / next pool.
    if hop.out_xrp {
        let drops = out_num.to_xrp_drops() as i64;
        credit_xrp(ctx, &hop.pool_id, -drops)?;
        if !skip_output_credit {
            if dest == taker {
                helpers::set_balance(taker_acct, helpers::get_balance(taker_acct) + drops as u64);
            } else {
                credit_xrp(ctx, dest, drops)?;
            }
        }
    } else {
        let pool_h = crate::amm_helpers::iou_holding_number(
            ctx.view,
            &hop.pool_id,
            &hop.out_iss,
            &hop.out_cur,
        );
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &hop.pool_id,
            &hop.out_iss,
            &hop.out_cur,
            &pool_h.sub(out_num),
        )?;
        if !skip_output_credit {
            let dest_h =
                crate::amm_helpers::iou_holding_number(ctx.view, dest, &hop.out_iss, &hop.out_cur);
            crate::amm_helpers::set_iou_holding(
                ctx.view,
                dest,
                &hop.out_iss,
                &hop.out_cur,
                &dest_h.add(out_num),
            )?;
        }
    }
    Ok(())
}

/// rippled `StrandFlow` output-limited case for a pure-AMM strand: reverse-price
/// the chain from the demanded output back to the required source input
/// (`swapAssetOut`, round IN up — rippled `DirectStep`/`BookStep` reverse pass),
/// and — when that input fits `budget` — apply the exact per-hop amounts so the
/// full `target_out` is delivered (`tesSUCCESS`). The intermediate amounts thread
/// exactly (each hop's input equals the previous hop's output), reproducing
/// rippled's reverse-rounded intermediates rather than the forward flow's
/// (which round the other way and would over-produce, then strand `TecPathPartial`).
///
/// Returns `Ok(Some((delivered, spent)))` after mutating the ledger, or
/// `Ok(None)` (no mutation) when any hop is not pure-AMM (a resting CLOB offer is
/// present or no AMM pool exists), the pool cannot meet the demand, or the
/// required input exceeds `budget` (input-limited). The caller's forward-only
/// flow then handles those cases.
fn reverse_amm_strand(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    boundaries: &[Value],
    target_out: &Leg,
    budget: &Leg,
) -> Result<Option<(Value, Value)>, TransactionResult> {
    use rxrpl_amount::number::Number;
    let n = boundaries.len();
    // Only multi-hop strands need the reverse pass: a single AMM hop already
    // delivers byte-exactly through the forward `amm_hop` (output-limited per
    // hop). Leave single-hop and degenerate chains to the forward flow.
    if n < 3 {
        return Ok(None);
    }

    // Gather a pure-AMM snapshot of every hop; bail to the forward flow on the
    // first hop carrying a resting CLOB offer or lacking an AMM pool.
    let mut hops: Vec<AmmHop> = Vec::with_capacity(n - 1);
    for h in 0..(n - 1) {
        let in_leg = Leg::parse(&boundaries[h]).ok_or(TransactionResult::TemBadAmount)?;
        let out_leg = Leg::parse(&boundaries[h + 1]).ok_or(TransactionResult::TemBadAmount)?;
        let inverse_book = keylet::book_dir(
            &in_leg.currency,
            &in_leg.issuer,
            &out_leg.currency,
            &out_leg.issuer,
        );
        if book_has_resting_offer(ctx, &inverse_book) {
            return Ok(None);
        }
        let amm_key = keylet::amm(
            &in_leg.currency,
            &in_leg.issuer,
            &out_leg.currency,
            &out_leg.issuer,
        );
        let Some(amm_bytes) = ctx.view.read(&amm_key) else {
            return Ok(None);
        };
        let Ok(amm): Result<Value, _> = serde_json::from_slice(&amm_bytes) else {
            return Ok(None);
        };
        let Some(pool_str) = amm.get("Account").and_then(|v| v.as_str()) else {
            return Ok(None);
        };
        let Ok(pool_id) = decode_account_id(pool_str) else {
            return Ok(None);
        };
        let tfee = amm.get("TradingFee").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
        let pool_in = pool_balance_number(
            ctx,
            &pool_id,
            in_leg.is_xrp,
            &in_leg.currency,
            &in_leg.issuer,
        );
        let pool_out = pool_balance_number(
            ctx,
            &pool_id,
            out_leg.is_xrp,
            &out_leg.currency,
            &out_leg.issuer,
        );
        if pool_in.is_zero() || pool_out.is_zero() {
            return Ok(None);
        }
        hops.push(AmmHop {
            pool_id,
            in_xrp: in_leg.is_xrp,
            out_xrp: out_leg.is_xrp,
            in_cur: in_leg.currency,
            in_iss: in_leg.issuer,
            out_cur: out_leg.currency,
            out_iss: out_leg.issuer,
            tfee,
            pool_in,
            pool_out,
        });
    }

    // Reverse pass: thread the demanded output back to the source input. Each
    // hop's required input becomes the previous hop's demanded output.
    let mut ins: Vec<Number> = vec![Number::ZERO; n - 1];
    let mut outs: Vec<Number> = vec![Number::ZERO; n - 1];
    let mut out_num = leg_to_number(target_out);
    for h in (0..(n - 1)).rev() {
        let hop = &hops[h];
        let in_num = match crate::amm_helpers::swap_asset_out(
            &hop.pool_in,
            &hop.pool_out,
            &out_num,
            hop.tfee,
            hop.in_xrp,
        ) {
            Some(v) if !v.is_zero() && !v.negative() => v,
            // Pool can't deliver the demand -> not output-limited; the forward
            // flow handles the (partial) input-limited case.
            _ => return Ok(None),
        };
        outs[h] = out_num;
        ins[h] = in_num;
        out_num = in_num;
    }

    // Output binds only when the required source input fits the budget; a larger
    // requirement means the strand is input-limited (forward flow handles it).
    // Compare at the Leg grid (drops / IOU mantissa) rather than via `Number`
    // subtraction, which panics on an exact-zero difference (in == budget).
    let in0_leg = number_to_leg(&ins[0], budget);
    if !leg_ge(budget, &in0_leg) {
        return Ok(None);
    }

    // Apply each hop with its exact reverse-computed (in, out): the first hop
    // debits the source, the last credits `dest`, interior hops move the shared
    // intermediate asset between adjacent pools.
    for h in 0..(n - 1) {
        let hop = &hops[h];
        apply_amm_move(
            ctx,
            taker,
            taker_acct,
            dest,
            hop,
            &ins[h],
            &outs[h],
            /*skip_input_debit=*/ h != 0,
            /*skip_output_credit=*/ h != n - 2,
        )?;
    }

    let spent_leg = number_to_leg(&ins[0], budget);
    let delivered_leg = number_to_leg(&outs[n - 2], target_out);
    Ok(Some((
        delivered_leg.with_amount(&delivered_leg.iou, delivered_leg.drops),
        spent_leg.with_amount(&spent_leg.iou, spent_leg.drops),
    )))
}

// ===========================================================================
// Multi-path Flow (rippled `Flow` + `AMMLiquidity` fib-chunked AMM consumption)
// ===========================================================================
//
// When two or more strands are live, rippled consumes a shared AMM pool in
// *fibonacci-sized synthetic offers* (one per multi-pass iteration) clamped to
// the competing CLOB quality, instead of one full output-limited swap. This is
// the difference between over-delivering (one swap) and the byte-exact chunked
// total rippled produces. The fib sizing + per-chunk quality clamp live in
// `super::flow`; the actual pool/holding mutation reuses `apply_amm_move` and
// the CLOB walk reuses `cross_book_hop` verbatim.

use super::flow::{AmmContext, AmmLiquidity};
use rxrpl_amount::number::Number;

/// rippled `Flow` loop bounds (StrandFlow.h).
const FLOW_MAX_TRIES: u32 = 1000;
const FLOW_MAX_OFFERS: u32 = 1500;

/// One hop of a resolved strand: the in→out boundary pair plus the AMM pool
/// (if any) and whether the book carries resting CLOB offers.
pub(crate) struct FlowHop {
    in_leg: Leg,
    out_leg: Leg,
    inverse_book: Hash256,
    has_clob: bool,
    /// AMM pool pseudo-account + trading fee, when a pool exists for the pair.
    amm_pool: Option<AccountId>,
    amm_tfee: u16,
    /// Frozen initial-pool snapshot for the fib seed (held stable for the whole
    /// payment); the live balances are refetched each pass.
    liquidity: Option<AmmLiquidity>,
}

/// A resolved strand = an ordered boundary chain of hops.
pub(crate) struct FlowStrand {
    hops: Vec<FlowHop>,
}

/// The realised result of executing one pass of a strand.
struct StrandPass {
    in_amt: Number,
    out_amt: Number,
    offers_used: u32,
}

/// The best-page (tip) quality of an order book, or `None` when empty.
fn book_tip_quality(ctx: &ApplyContext<'_>, inverse_book: &Hash256) -> Option<u64> {
    let probe = book_dir_with_quality(inverse_book, 0);
    let dir_key = ctx.view.succ(&probe)?;
    if dir_key.as_bytes()[0..24] != inverse_book.as_bytes()[0..24] {
        return None;
    }
    Some(u64::from_be_bytes(
        dir_key.as_bytes()[24..32].try_into().unwrap(),
    ))
}

/// Build a strand from a boundary chain, snapshotting each hop's AMM pool (with
/// its frozen initial balances) and CLOB presence. Returns `None` if any hop has
/// neither an AMM pool nor a resting offer (a dead hop).
pub(crate) fn build_flow_strand(
    ctx: &ApplyContext<'_>,
    boundaries: &[Value],
) -> Option<FlowStrand> {
    let n = boundaries.len();
    if n < 2 {
        return None;
    }
    let mut hops = Vec::with_capacity(n - 1);
    for h in 0..(n - 1) {
        let in_leg = Leg::parse(&boundaries[h])?;
        let out_leg = Leg::parse(&boundaries[h + 1])?;
        let inverse_book = keylet::book_dir(
            &in_leg.currency,
            &in_leg.issuer,
            &out_leg.currency,
            &out_leg.issuer,
        );
        let has_clob = book_has_resting_offer(ctx, &inverse_book);

        let amm_key = keylet::amm(
            &in_leg.currency,
            &in_leg.issuer,
            &out_leg.currency,
            &out_leg.issuer,
        );
        let (amm_pool, amm_tfee, liquidity) = match ctx
            .view
            .read(&amm_key)
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        {
            Some(amm) => {
                let pool_id = amm
                    .get("Account")
                    .and_then(|v| v.as_str())
                    .and_then(|s| decode_account_id(s).ok());
                let tfee = amm.get("TradingFee").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                match pool_id {
                    Some(pid) => {
                        let pool_in = pool_balance_number(
                            ctx,
                            &pid,
                            in_leg.is_xrp,
                            &in_leg.currency,
                            &in_leg.issuer,
                        );
                        let pool_out = pool_balance_number(
                            ctx,
                            &pid,
                            out_leg.is_xrp,
                            &out_leg.currency,
                            &out_leg.issuer,
                        );
                        if pool_in.is_zero() || pool_out.is_zero() {
                            (None, 0, None)
                        } else {
                            (
                                Some(pid),
                                tfee,
                                Some(AmmLiquidity {
                                    in_is_xrp: in_leg.is_xrp,
                                    out_is_xrp: out_leg.is_xrp,
                                    tfee,
                                    initial_pool_in: pool_in,
                                    initial_pool_out: pool_out,
                                }),
                            )
                        }
                    }
                    None => (None, 0, None),
                }
            }
            None => (None, 0, None),
        };

        // NOTE: a hop with neither an AMM pool nor a resting CLOB offer is a
        // *dry* hop — the strand cannot deliver — but the strand is still
        // syntactically valid and its PRESENCE keeps `multi_path > 1`, which is
        // what triggers rippled's fib-chunked AMM consumption on the OTHER live
        // strand. So we keep it (it simply returns no delivery each pass) rather
        // than dropping it (which would collapse a multi-path payment to a
        // single full swap).
        hops.push(FlowHop {
            in_leg,
            out_leg,
            inverse_book,
            has_clob,
            amm_pool,
            amm_tfee,
            liquidity,
        });
    }
    Some(FlowStrand { hops })
}

/// A strand's a-priori quality upper bound as a `Number` rate (in/out, lower is
/// better): the product of each hop's best-possible rate. Composing left→right,
/// the intermediate magnitudes cancel so the product is the overall
/// input-per-output rate. Used to rank live strands best-first each pass (rippled
/// `StrandFlow.h:444-496` -> `BookStep::qualityUpperBound` -> `getTipQuality`).
///
/// Each hop's rate is the better of its CLOB tip and its AMM offer. The AMM
/// contribution is the *fib-offer* quality (`getAMMOffer(nullopt).quality()`,
/// which folds in the trading fee and the chunk's price impact for the CURRENT
/// `amm_ctx` iteration), NOT the raw spot price — the raw spot over-ranks a
/// pure-AMM strand and flips the first-survivor pick (StrandFlow `getTipQuality`,
/// go-xrpl `step_book.go:869`).
fn strand_quality_ub(ctx: &ApplyContext<'_>, strand: &FlowStrand, amm_ctx: &AmmContext) -> Number {
    let mut rate = Number::from_int(1);
    for hop in &strand.hops {
        // AMM fib-offer quality for this iteration (fee + impact), as rippled's
        // `getTipQuality` uses `getAMMOffer(nullopt).quality()`.
        let amm_rate = match (hop.liquidity.as_ref(), hop.amm_pool.as_ref()) {
            (Some(liq), Some(pool)) => {
                let pin = pool_balance_number(
                    ctx,
                    pool,
                    hop.in_leg.is_xrp,
                    &hop.in_leg.currency,
                    &hop.in_leg.issuer,
                );
                let pout = pool_balance_number(
                    ctx,
                    pool,
                    hop.out_leg.is_xrp,
                    &hop.out_leg.currency,
                    &hop.out_leg.issuer,
                );
                liq.get_offer(&pin, &pout, None, amm_ctx)
                    .and_then(|o| from_rate(o.quality).ok())
                    .map(|r| Number::from_iou(&r))
            }
            _ => None,
        };
        // CLOB best-tip rate (in/out) from the book directory quality.
        let clob_rate = book_tip_quality(ctx, &hop.inverse_book)
            .and_then(|q| from_rate(q).ok())
            .map(|r| Number::from_iou(&r));
        let hop_rate = match (amm_rate, clob_rate) {
            (Some(a), Some(c)) => {
                if num_le(&a, &c) {
                    a
                } else {
                    c
                }
            }
            (Some(a), None) => a,
            (None, Some(c)) => c,
            (None, None) => Number::from_int(1),
        };
        rate = rate.mul(&hop_rate);
    }
    rate
}

/// Sum a slice of `Number`s smallest-magnitude first (rippled
/// `StrandFlow.h:619-624`): never a running subtraction. Stable accumulation
/// keeps the byte-exact total independent of pass order.
fn sum_smallest_to_largest(v: &[Number]) -> Number {
    let mut sorted: Vec<Number> = v.to_vec();
    sorted.sort_by(|a, b| {
        let d = a.sub(b);
        if d.is_zero() {
            std::cmp::Ordering::Equal
        } else if d.negative() {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
    let mut acc = Number::ZERO;
    for n in &sorted {
        acc = acc.add(n);
    }
    acc
}

/// `a >= b` for two `Number`s.
fn num_ge(a: &Number, b: &Number) -> bool {
    !num_gt(b, a)
}

/// Snapshot a hop's AMM pool as an [`AmmHop`] at the current (live) balances.
fn hop_amm_snapshot(ctx: &ApplyContext<'_>, hop: &FlowHop) -> Option<(AmmHop, Number, Number)> {
    let pool = hop.amm_pool.as_ref()?;
    let pool_in = pool_balance_number(
        ctx,
        pool,
        hop.in_leg.is_xrp,
        &hop.in_leg.currency,
        &hop.in_leg.issuer,
    );
    let pool_out = pool_balance_number(
        ctx,
        pool,
        hop.out_leg.is_xrp,
        &hop.out_leg.currency,
        &hop.out_leg.issuer,
    );
    let amm_hop = AmmHop {
        pool_id: *pool,
        in_xrp: hop.in_leg.is_xrp,
        out_xrp: hop.out_leg.is_xrp,
        in_cur: hop.in_leg.currency,
        in_iss: hop.in_leg.issuer,
        out_cur: hop.out_leg.currency,
        out_iss: hop.out_leg.issuer,
        tfee: hop.amm_tfee,
        pool_in,
        pool_out,
    };
    Some((amm_hop, pool_in, pool_out))
}

/// Price one CLOB tip band in the reverse direction: how much input the resting
/// offers need to deliver `demanded` output, and what they actually deliver
/// (`min(band capacity, demanded)`). The walk mutates the view (consumes /
/// reaps offers) so it runs on a checkpoint that is immediately rolled back; the
/// forward pass re-walks and commits the real consumption. Returns
/// `(input_needed, output_delivered)` as `Number`s, or `None` when the band is
/// dry. The `skip_*` flags mirror the forward application so the grossed/netted
/// transfer-fee amounts match.
#[allow(clippy::too_many_arguments)]
fn price_clob_band_reverse(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    hop: &FlowHop,
    demanded: &Number,
    is_first: bool,
    is_last: bool,
) -> Option<(Number, Number)> {
    let demand_leg = number_to_leg(demanded, &hop.out_leg);
    let budget_leg = unbounded_leg(&hop.in_leg);
    let cp = ctx.view.checkpoint();
    let res = cross_book_hop(
        ctx,
        taker,
        taker_acct,
        dest,
        &demand_leg,
        &budget_leg,
        /*skip_input_debit=*/ !is_first,
        /*skip_output_credit=*/ !is_last,
        /*single_band=*/ true,
    );
    ctx.view.rollback(cp);
    let (delivered, spent) = res.ok()?;
    if delivered.is_zero() || spent.is_zero() {
        return None;
    }
    Some((leg_to_number(&spent), leg_to_number(&delivered)))
}

/// Execute ONE pass of a (multi-hop, AMM and/or CLOB) strand with rippled's
/// reverse-then-forward `StrandFlow` algorithm (StrandFlow.h:80-281).
///
/// Each hop consumes ONE quality band per pass. rippled's `BookStep` tries the
/// AMM FIRST at the CLOB tip quality ([`AmmLiquidity::get_offer`]); the AMM
/// declines (returns `None`) when its fib chunk is no better than the resting
/// book, and the band is then filled by the CLOB tip alone (a one-band
/// [`cross_book_hop`] walk). A pure-AMM hop is the fib chunk; a pure-CLOB hop is
/// the tip band; a dry hop (neither) fails the strand this pass.
///
/// **Reverse pass**: from the demanded strand output `remaining_out`, pull the
/// demand back through every hop. An AMM hop's chunk is clamped to the demand via
/// `limit_out`; a CLOB hop is priced by a one-band `cross_book_hop` on a
/// checkpoint that is immediately rolled back. The chunk/band's required input
/// becomes the upstream hop's demand. The hop whose band cannot meet the
/// back-propagated demand is the *limiting step*. The source (hop 0) is also
/// limited when its required input exceeds the `remaining_in` budget.
///
/// **Forward pass** (mutates the checkpointed view): hops `0..=limiting` keep
/// their reverse-priced amounts (which already thread — each hop's output equals
/// the next hop's input); hops past the limiting step are re-priced forward
/// against the reduced carry (AMM via `limit_in`, CLOB by an input-limited
/// one-band walk), so the intermediate asset is conserved exactly. The AMM
/// mutation reuses [`apply_amm_move`]; the CLOB mutation/reap reuses
/// [`cross_book_hop`]. The SHARED `amm_ctx` threads `curIters` across all AMM
/// hops of the strand.
///
/// Returns the realised `(source-in, delivered-out)`, or `None` when the strand
/// cannot deliver this pass (a pool is frozen, a hop is dry, or the demand cannot
/// be priced).
#[allow(clippy::too_many_arguments)]
fn execute_strand_pass(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    strand: &FlowStrand,
    remaining_in: &Number,
    remaining_out: &Number,
    amm_ctx: &mut AmmContext,
) -> Option<StrandPass> {
    let trace = std::env::var("RXRPL_FLOW_TRACE").is_ok();
    let n = strand.hops.len();
    if n == 0 {
        return None;
    }

    // Per-hop AMM snapshot (live pool balances at the start of this pass), or
    // `None` for a pure-CLOB hop. A hop with neither AMM nor CLOB is dry.
    let mut amm_hops: Vec<Option<AmmHop>> = Vec::with_capacity(n);
    for hop in &strand.hops {
        amm_hops.push(hop_amm_snapshot(ctx, hop).map(|(a, _, _)| a));
    }

    // Whether a hop consumes its AMM fib chunk this pass, or its CLOB tip band.
    // rippled's `BookStep` tries the AMM FIRST at the CLOB tip quality; the AMM
    // declines (`get_offer` -> None) when it is no better than the book, and the
    // band is then filled by the resting CLOB offers alone.
    #[derive(Clone, Copy, PartialEq)]
    enum HopKindR {
        Amm,
        Clob,
    }

    // --- REVERSE PASS: back-propagate the demand, find the limiting step. ---
    let mut ins: Vec<Number> = vec![Number::ZERO; n];
    let mut outs: Vec<Number> = vec![Number::ZERO; n];
    let mut kinds: Vec<HopKindR> = vec![HopKindR::Amm; n];
    // The lowest-index hop whose band cannot meet the demand pulled back to it;
    // `n` means "no interior limit" (only the source budget may bind).
    let mut limiting = n;
    let mut demanded = *remaining_out;
    for h in (0..n).rev() {
        let hop = &strand.hops[h];
        let is_first = h == 0;
        let is_last = h == n - 1;
        let clob_quality = book_tip_quality(ctx, &hop.inverse_book);

        // Try the AMM fib chunk first, gated to the CLOB tip quality.
        let amm_offer = match (hop.liquidity.as_ref(), amm_hops[h].as_ref()) {
            (Some(liq), Some(amm)) => {
                liq.get_offer(&amm.pool_in, &amm.pool_out, clob_quality, amm_ctx)
            }
            _ => None,
        };

        if let Some(offer) = amm_offer {
            kinds[h] = HopKindR::Amm;
            if num_ge(&offer.out_num, &demanded) {
                // The chunk covers the demand: clamp output to demand at constant
                // quality (`ceilOutStrict`, round the cost up).
                let (i, o) = offer.limit_out(&demanded, true);
                ins[h] = i;
                outs[h] = o;
            } else {
                // The chunk is smaller than the demand: this hop limits the pass.
                ins[h] = offer.in_num;
                outs[h] = offer.out_num;
                limiting = h;
            }
        } else if clob_quality.is_some() {
            // AMM declined (or absent) but the book has resting offers: price the
            // CLOB tip band for the demand pulled to this hop. Done on a
            // checkpoint that is rolled back — the forward pass re-runs the walk
            // and commits the offer consumption / reaps.
            kinds[h] = HopKindR::Clob;
            let (i, o) = price_clob_band_reverse(
                ctx, taker, taker_acct, dest, hop, &demanded, is_first, is_last,
            )?;
            ins[h] = i;
            outs[h] = o;
            // The band delivered less than demanded -> this hop limits the pass.
            if num_gt(&demanded, &o) {
                limiting = h;
            }
        } else {
            // Dry hop: neither AMM liquidity nor a resting CLOB offer.
            return None;
        }
        if ins[h].is_zero() || outs[h].is_zero() || ins[h].negative() {
            return None;
        }
        demanded = ins[h];
    }

    // The source (hop 0) is input-limited when its required input exceeds the
    // remaining SendMax budget: drive it forward from the budget instead.
    let source_capped = num_gt(&ins[0], remaining_in);
    if source_capped {
        limiting = 0;
    }

    // --- FORWARD PASS: apply, threading the intermediate so it conserves. ---
    let mut source_in = ins[0];
    let mut carry = Number::ZERO;
    for h in 0..n {
        let hop = &strand.hops[h];
        let is_first = h == 0;
        let is_last = h == n - 1;
        let clob_q = book_tip_quality(ctx, &hop.inverse_book);

        match kinds[h] {
            HopKindR::Amm => {
                let amm = amm_hops[h].as_ref()?;
                let (in_num, out_num) = if is_first && source_capped {
                    // Source-limited: re-price hop 0 forward from the budget.
                    let liq = hop.liquidity.as_ref()?;
                    let offer = liq.get_offer(&amm.pool_in, &amm.pool_out, clob_q, amm_ctx)?;
                    offer.limit_in(remaining_in, false)
                } else if h <= limiting {
                    // Reverse-priced; conserved by construction (outs[h-1] == ins[h]).
                    (ins[h], outs[h])
                } else {
                    // Past the limiting step: re-price forward against the reduced
                    // carry (`limitStepIn`, round the output down), keeping quality.
                    let liq = hop.liquidity.as_ref()?;
                    let offer = liq.get_offer(&amm.pool_in, &amm.pool_out, clob_q, amm_ctx)?;
                    if num_ge(&offer.in_num, &carry) {
                        offer.limit_in(&carry, false)
                    } else {
                        (offer.in_num, offer.out_num)
                    }
                };
                if in_num.is_zero() || out_num.is_zero() || in_num.negative() || out_num.negative()
                {
                    return None;
                }
                // Conservation: an interior hop takes exactly what the previous hop
                // delivered (`carry`); otherwise the intermediate does not balance.
                let in_num = if is_first { in_num } else { carry };
                apply_amm_move(
                    ctx, taker, taker_acct, dest, amm, &in_num, &out_num,
                    /*skip_input_debit=*/ !is_first, /*skip_output_credit=*/ !is_last,
                )
                .ok()?;
                amm_ctx.set_amm_used();
                if trace {
                    eprintln!(
                        "[flow]     hop{h} AMM in={} out={} (iter={} limiting={limiting})",
                        in_num.to_iou().to_decimal_string(),
                        out_num.to_iou().to_decimal_string(),
                        amm_ctx.cur_iters(),
                    );
                }
                if is_first {
                    source_in = in_num;
                }
                carry = out_num;
            }
            HopKindR::Clob => {
                // Drive the CLOB band: at/below the limiting step deliver the
                // reverse-priced output; above it (or source-capped) drive by the
                // available input (`BookStep::fwd`, input-limited).
                let (demand_leg, budget_leg) = if is_first && source_capped {
                    (
                        unbounded_leg(&hop.out_leg),
                        number_to_leg(remaining_in, &hop.in_leg),
                    )
                } else if h <= limiting {
                    let budget = if is_first {
                        number_to_leg(&ins[0], &hop.in_leg)
                    } else {
                        unbounded_leg(&hop.in_leg)
                    };
                    (number_to_leg(&outs[h], &hop.out_leg), budget)
                } else {
                    (
                        unbounded_leg(&hop.out_leg),
                        number_to_leg(&carry, &hop.in_leg),
                    )
                };
                let (delivered, spent) = cross_book_hop(
                    ctx,
                    taker,
                    taker_acct,
                    dest,
                    &demand_leg,
                    &budget_leg,
                    /*skip_input_debit=*/ !is_first,
                    /*skip_output_credit=*/ !is_last,
                    /*single_band=*/ true,
                )
                .ok()?;
                if delivered.is_zero() || spent.is_zero() {
                    return None;
                }
                if trace {
                    eprintln!(
                        "[flow]     hop{h} CLOB in={} out={} (limiting={limiting})",
                        leg_to_number(&spent).to_iou().to_decimal_string(),
                        leg_to_number(&delivered).to_iou().to_decimal_string(),
                    );
                }
                if is_first {
                    source_in = leg_to_number(&spent);
                }
                carry = leg_to_number(&delivered);
            }
        }
    }

    if source_in.is_zero() || carry.is_zero() {
        return None;
    }
    Some(StrandPass {
        in_amt: source_in,
        out_amt: carry,
        offers_used: 0,
    })
}

/// rippled `Flow` multi-pass loop (sorted / first-survivor, `fixFlowSortStrands`
/// active). Each pass ranks the live strands best-quality-first, recomputes the
/// `multi_path` toggle from the live count, then picks the FIRST strand that
/// delivers (checkpointing before each trial, rolling back failed ones, keeping
/// the winner's mutation). `amm_ctx` is cleared per trial and advanced once per
/// committed winner — this drives the fib index. Remaining in/out are always
/// recomputed via `sum_smallest_to_largest`, never a running subtraction.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flow_multi(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    strands: &[FlowStrand],
    deliver_req: &Number,
    send_max: &Number,
    amm_ctx: &mut AmmContext,
) -> (Number, Number) {
    let trace = std::env::var("RXRPL_FLOW_TRACE").is_ok();
    if trace {
        eprintln!(
            "[flow] ENTER strands={} deliver_req={} send_max={}",
            strands.len(),
            deliver_req.to_iou().to_decimal_string(),
            send_max.to_iou().to_decimal_string(),
        );
        for (si, s) in strands.iter().enumerate() {
            let desc: Vec<String> = s
                .hops
                .iter()
                .map(|h| format!("amm={} clob={}", h.amm_pool.is_some(), h.has_clob))
                .collect();
            eprintln!(
                "[flow]   strand {si}: {} hops [{}]",
                s.hops.len(),
                desc.join(" | ")
            );
        }
    }
    let mut saved_ins: Vec<Number> = Vec::new();
    let mut saved_outs: Vec<Number> = Vec::new();
    let mut offers_considered = 0u32;

    for cur_try in 0..FLOW_MAX_TRIES {
        let remaining_out = {
            let d = deliver_req.sub(&sum_smallest_to_largest(&saved_outs));
            if d.is_zero() || d.negative() {
                break;
            }
            d
        };
        let remaining_in = {
            let r = send_max.sub(&sum_smallest_to_largest(&saved_ins));
            if r.is_zero() || r.negative() {
                break;
            }
            r
        };

        // (A) activateNext: rank ALL strands best-quality-first each pass.
        //
        // Unlike rippled's `ActiveStrands` (which drops a strand once its rev
        // pass returns dry, shrinking the active set), we keep every strand live
        // for the whole payment. A strand that delivers nothing this pass simply
        // loses the first-survivor race, but its PRESENCE keeps `multi_path > 1`
        // — which is what sustains the fib-chunked AMM consumption across all
        // passes (a shared AMM first hop drains in fib increments only while two
        // or more strands are live). Dropping the dry strand would collapse the
        // payment to a single full swap (over-delivery).

        let mut cur: Vec<usize> = (0..strands.len()).collect();
        if cur.len() > 1 {
            cur.sort_by(|&a, &b| {
                let ra = strand_quality_ub(ctx, &strands[a], amm_ctx);
                let rb = strand_quality_ub(ctx, &strands[b], amm_ctx);
                let d = ra.sub(&rb);
                if d.is_zero() {
                    std::cmp::Ordering::Equal
                } else if d.negative() {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            });
        }
        if cur.is_empty() {
            break;
        }
        if trace {
            let qs: Vec<String> = cur
                .iter()
                .map(|&i| {
                    format!(
                        "s{i}={}",
                        strand_quality_ub(ctx, &strands[i], amm_ctx)
                            .to_iou()
                            .to_decimal_string()
                    )
                })
                .collect();
            eprintln!("[flow]   try={cur_try} rank [{}]", qs.join(" "));
        }
        // (B) multiPath toggle, from the live strand count (seeded at setup like
        // rippled `Flow.cpp:106` = `strands.size() > 1`).
        amm_ctx.set_multi_path(cur.len() > 1);

        // (D) pick the FIRST surviving strand (checkpoint per trial; roll back
        // losers; keep the winner's mutation).
        let mut winner: Option<(usize, StrandPass)> = None;
        for &si in cur.iter() {
            let cp = ctx.view.checkpoint();
            amm_ctx.clear();
            match execute_strand_pass(
                ctx,
                taker,
                taker_acct,
                dest,
                &strands[si],
                &remaining_in,
                &remaining_out,
                amm_ctx,
            ) {
                Some(res) if !res.out_amt.is_zero() => {
                    offers_considered += res.offers_used;
                    winner = Some((si, res));
                    break;
                }
                other => {
                    if let Some(r) = other {
                        offers_considered += r.offers_used;
                    }
                    if trace {
                        eprintln!("[flow]   try={cur_try} strand={si} FAILED pass");
                    }
                    ctx.view.rollback(cp);
                }
            }
        }

        let Some((si, res)) = winner else {
            break;
        };
        if trace {
            eprintln!(
                "[flow] try={cur_try} strand={si} multi={} amm_iter={} in={} out={}",
                amm_ctx.multi_path(),
                amm_ctx.cur_iters(),
                res.in_amt.to_iou().to_decimal_string(),
                res.out_amt.to_iou().to_decimal_string(),
            );
        }
        saved_ins.push(res.in_amt);
        saved_outs.push(res.out_amt);
        // (E) advance the fib counter once per committed winner.
        amm_ctx.update();

        if offers_considered >= FLOW_MAX_OFFERS {
            break;
        }
    }

    (
        sum_smallest_to_largest(&saved_outs),
        sum_smallest_to_largest(&saved_ins),
    )
}

/// `a + b` for like-typed legs (same asset).
fn leg_add(a: &Leg, b: &Leg) -> Leg {
    let mut out = a.clone();
    if a.is_xrp {
        out.drops = a.drops + b.drops;
    } else {
        out.iou = IOUAmount::add(&a.iou, &b.iou).unwrap_or(a.iou);
    }
    out
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

/// `a - b` for like-typed legs, rounding the IOU result to nearest (modern
/// Flow / `Number` semantics) instead of the legacy truncating [`leg_sub`].
/// XRP drops are exact integers, so the two agree on the native side.
fn leg_sub_round(a: &Leg, b: &Leg) -> Leg {
    let mut out = a.clone();
    if a.is_xrp {
        out.drops = a.drops - b.drops;
    } else {
        out.iou = IOUAmount::add_round(&a.iou, &b.iou.negate()).unwrap_or(IOUAmount::ZERO);
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
    round: bool,
    skip_taker_debit: bool,
) -> Result<(), TransactionResult> {
    if amount.is_xrp {
        if !skip_taker_debit {
            let bal = helpers::get_balance(taker_acct) as i64 - amount.drops;
            if bal < 0 {
                return Err(TransactionResult::TecUnfundedOffer);
            }
            helpers::set_balance(taker_acct, bal as u64);
        }
        return credit_xrp(ctx, owner, amount.drops);
    }
    if !skip_taker_debit {
        let rate = transfer_rate(ctx, &amount.issuer);
        let gross = grossed(&amount.iou, &rate);
        credit_line(
            ctx,
            taker,
            &amount.issuer,
            &amount.currency,
            &gross.negate(),
            round,
        )?;
    }
    credit_line(
        ctx,
        owner,
        &amount.issuer,
        &amount.currency,
        &amount.iou,
        round,
    )
}

/// Move the offer owner's output to `recipient`: XRP via balances, IOU via the
/// grossed owner debit / net recipient credit (the difference is the burned
/// fee). When `recipient` is the taker, its XRP credit goes through `taker_acct`
/// (the caller's working copy) so the apply's final account write does not
/// clobber it; a distinct recipient (cross-currency payment to another account)
/// is credited through the view instead.
///
/// This is the legacy Taker (OfferCreate crossing) path: `amount` is the NET
/// the recipient receives and the offer's TakerGets reduction, while the owner
/// is unconditionally debited the grossed-up amount (`amount * transfer_rate`).
/// The cross-currency Payment path uses [`pay_out_gross`] instead, where the
/// fee is charged out of a gross input and skipped when the issuer is a party.
fn pay_out(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    owner: &AccountId,
    recipient: &AccountId,
    amount: &Leg,
    round: bool,
) -> Result<(), TransactionResult> {
    if amount.is_xrp {
        credit_xrp(ctx, owner, -amount.drops)?;
        if recipient == taker {
            helpers::set_balance(
                taker_acct,
                helpers::get_balance(taker_acct) + amount.drops as u64,
            );
        } else {
            credit_xrp(ctx, recipient, amount.drops)?;
        }
        return Ok(());
    }
    let rate = transfer_rate(ctx, &amount.issuer);
    let gross = grossed(&amount.iou, &rate);
    // When the offer owner is itself the output issuer it has no trust line on
    // its side to debit — it issues its own IOU directly (rippled's rippleCredit
    // skips the issuer's self-line). Debiting the nonexistent self-line otherwise
    // fails the whole crossing with tecPATH_DRY and the taker's offer rests
    // uncrossed instead of consuming the issuer's resting offers.
    if owner != &amount.issuer {
        credit_line(
            ctx,
            owner,
            &amount.issuer,
            &amount.currency,
            &gross.negate(),
            round,
        )?;
    }
    credit_line(
        ctx,
        recipient,
        &amount.issuer,
        &amount.currency,
        &amount.iou,
        round,
    )
}

/// Move the offer owner's output to `recipient` and return the NET delivered
/// (what `recipient` actually receives). Used by the cross-currency Payment
/// path, where `amount` is the GROSS the offer provides — exactly the offer's
/// TakerGets reduction and the owner's debit. The output issuer's transfer fee
/// is charged out of that gross: the recipient receives `amount / transfer_rate`
/// rounded DOWN (rippled never delivers more), and the difference is the burned
/// fee. rippled skips the fee whenever the issuer is itself a party (owner or
/// recipient), so the net then equals the gross. XRP carries no transfer fee, so
/// net == gross there. When `recipient` is the taker, its XRP credit goes
/// through `taker_acct` (the caller's working copy) so the apply's final account
/// write does not clobber it; a distinct recipient (cross-currency payment to
/// another account) is credited through the view instead.
#[allow(clippy::too_many_arguments)]
fn pay_out_gross(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    owner: &AccountId,
    recipient: &AccountId,
    amount: &Leg,
    round: bool,
    skip_recipient_credit: bool,
) -> Result<Leg, TransactionResult> {
    if amount.is_xrp {
        credit_xrp(ctx, owner, -amount.drops)?;
        if !skip_recipient_credit {
            if recipient == taker {
                helpers::set_balance(
                    taker_acct,
                    helpers::get_balance(taker_acct) + amount.drops as u64,
                );
            } else {
                credit_xrp(ctx, recipient, amount.drops)?;
            }
        }
        return Ok(amount.clone());
    }
    // The fee applies only when neither party is the issuer (rippled charges no
    // transfer fee on issuance/redemption). When it applies, the recipient nets
    // `gross / rate` rounded down; otherwise net == gross. For an intermediate
    // hop (`skip_recipient_credit`) the output funds the next book directly, so
    // there is no fee-charged recipient and the net carried forward is the gross.
    let fee_applies =
        !skip_recipient_credit && amount.issuer != *owner && amount.issuer != *recipient;
    let net = if fee_applies {
        let rate = transfer_rate(ctx, &amount.issuer);
        IOUAmount::div_round(&amount.iou, &rate, /*round_up*/ false).unwrap_or(amount.iou)
    } else {
        amount.iou
    };
    // Debit the owner the full gross it provides (the offer's TakerGets move).
    // When a party to the delivery is itself the output issuer it has no trust
    // line on its side to move — it issues (owner) or redeems (recipient) its own
    // IOU directly (rippled's rippleCredit skips the issuer self-line). Debiting a
    // nonexistent self-line otherwise fails the crossing with tecPATH_DRY, exactly
    // as `pay_out` did for issuer-owned offers before the guard.
    if amount.issuer != *owner {
        credit_line(
            ctx,
            owner,
            &amount.issuer,
            &amount.currency,
            &amount.iou.negate(),
            round,
        )?;
    }
    if !skip_recipient_credit && amount.issuer != *recipient {
        credit_line(
            ctx,
            recipient,
            &amount.issuer,
            &amount.currency,
            &net,
            round,
        )?;
    }
    let mut net_leg = amount.clone();
    net_leg.iou = net;
    Ok(net_leg)
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
/// The owner's spendable funds in the offer's `gets` currency, as a `Leg`.
/// A zero result means the offer is unfunded (the caller reaps it); otherwise
/// the fill is clamped to this amount (rippled fills against owner funds rather
/// than requiring the whole offer to be funded). XRP uses the raw balance; an
/// IOU issuer can always issue its own currency (not a binding constraint); an
/// IOU holder is bounded by its trust-line balance.
/// rippled `isDeepFrozen(view, account, asset)`: a line the account holds for
/// `asset` is deep-frozen when either side set the deep-freeze flag (a
/// deep-frozen line can neither receive nor send). XRP and the issuer's own
/// asset are never deep-frozen.
fn is_deep_frozen(ctx: &mut ApplyContext<'_>, account: &AccountId, asset: &Leg) -> bool {
    const LSF_LOW_DEEP_FREEZE: u64 = 0x0200_0000;
    const LSF_HIGH_DEEP_FREEZE: u64 = 0x0400_0000;
    if asset.is_xrp || &asset.issuer == account {
        return false;
    }
    let Some(tl_bytes) =
        ctx.view
            .read(&keylet::trust_line(account, &asset.issuer, &asset.currency))
    else {
        return false;
    };
    let Ok(tl) = serde_json::from_slice::<Value>(&tl_bytes) else {
        return false;
    };
    let flags = tl.get("Flags").and_then(Value::as_u64).unwrap_or(0);
    flags & (LSF_LOW_DEEP_FREEZE | LSF_HIGH_DEEP_FREEZE) != 0
}

fn owner_funds_leg(ctx: &mut ApplyContext<'_>, owner: &AccountId, gets: &Leg) -> Leg {
    let zero = Leg {
        is_xrp: gets.is_xrp,
        drops: 0,
        iou: IOUAmount::ZERO,
        currency: gets.currency,
        issuer: gets.issuer,
    };
    if gets.is_xrp {
        let key = keylet::account(owner);
        let acct = ctx
            .view
            .read(&key)
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let bal = acct
            .as_ref()
            .and_then(|a| {
                a.get("Balance")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
            })
            .unwrap_or(0);
        // rippled `accountFunds` for XRP is `xrpLiquid(owner, 0)` = balance minus
        // the account reserve, clamped at zero: an offer can only sell the owner's
        // spendable XRP, not the reserve-locked portion. Using the raw balance let
        // a reserve-constrained offer over-fill (execute instead of being reaped).
        let owner_count = acct.as_ref().map(helpers::get_owner_count).unwrap_or(0);
        let liquid = bal - ctx.fees.account_reserve(owner_count) as i64;
        if liquid <= 0 {
            return zero;
        }
        return Leg {
            is_xrp: true,
            drops: liquid,
            iou: IOUAmount::ZERO,
            currency: gets.currency,
            issuer: gets.issuer,
        };
    }
    if &gets.issuer == owner {
        return gets.clone();
    }
    let tl_key = keylet::trust_line(owner, &gets.issuer, &gets.currency);
    let Some(tl_bytes) = ctx.view.read(&tl_key) else {
        return zero;
    };
    let Ok(tl) = serde_json::from_slice::<Value>(&tl_bytes) else {
        return zero;
    };
    let raw_str = tl
        .get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let raw_f: f64 = raw_str.parse().unwrap_or(0.0);
    // Balance is stored from the low account's perspective; the holder's view
    // flips sign when the issuer is the low account.
    let issuer_is_low = gets.issuer.as_bytes() < owner.as_bytes();
    let holder_positive = if issuer_is_low {
        raw_f < 0.0
    } else {
        raw_f > 0.0
    };
    if !holder_positive {
        return zero;
    }
    // rippled `accountFundsHelper` prices a maker's IOU with
    // `ZeroIfFrozen`/`ZeroIfUnauthorized`: a maker cannot deliver an IOU it
    // can't move, so the offer is found unfunded (0) and reaped. Mirror that —
    // the issuer global-froze, or froze this line, or requires auth and hasn't
    // authorized it.
    const LSF_LOW_FREEZE: u64 = 0x0040_0000;
    const LSF_HIGH_FREEZE: u64 = 0x0080_0000;
    const LSF_LOW_AUTH: u64 = 0x0004_0000;
    const LSF_HIGH_AUTH: u64 = 0x0008_0000;
    const LSF_GLOBAL_FREEZE: u64 = 0x0040_0000;
    const LSF_REQUIRE_AUTH: u64 = 0x0004_0000;
    let tl_flags = tl.get("Flags").and_then(Value::as_u64).unwrap_or(0);
    let issuer_flags = ctx
        .view
        .read(&keylet::account(&gets.issuer))
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|a| a.get("Flags").and_then(Value::as_u64))
        .unwrap_or(0);
    let line_frozen = if issuer_is_low {
        tl_flags & LSF_LOW_FREEZE != 0
    } else {
        tl_flags & LSF_HIGH_FREEZE != 0
    };
    if line_frozen || issuer_flags & LSF_GLOBAL_FREEZE != 0 {
        return zero;
    }
    if issuer_flags & LSF_REQUIRE_AUTH != 0 {
        let authorized = if issuer_is_low {
            tl_flags & LSF_LOW_AUTH != 0
        } else {
            tl_flags & LSF_HIGH_AUTH != 0
        };
        if !authorized {
            return zero;
        }
    }
    let mag = raw_str.trim_start_matches('-');
    Leg {
        is_xrp: false,
        drops: 0,
        iou: IOUAmount::from_decimal_string(mag).unwrap_or(IOUAmount::ZERO),
        currency: gets.currency,
        issuer: gets.issuer,
    }
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

#[cfg(test)]
mod tick_round_tests {
    use super::{rebuild_iou, round_drops_half_even};
    use rxrpl_amount::IOUAmount;

    // The tick re-derivation of an XRP side yields a fractional drops magnitude;
    // rebuild_iou must round it onto an integer drop the way rippled's
    // `STAmount(XRP)` does (round half to even), not truncate. Mainnet tx
    // DFF0E4CB snaps TakerGets 3654519 -> 3654430 (re-derived 3654430.39499415,
    // rounds down); tx 3CB55737 snaps 8543180 -> 8543055 (re-derived
    // 8543054.951085025, rounds up — truncation produced 8543054 and diverged).
    #[test]
    fn rebuild_iou_rounds_xrp_drops_half_even() {
        let original = serde_json::json!("3654519");
        let redrived = IOUAmount::from_decimal_string("3654430.39499415").unwrap();
        assert_eq!(
            rebuild_iou(&original, &redrived),
            Some(serde_json::json!("3654430"))
        );

        let original = serde_json::json!("8543180");
        let redrived = IOUAmount::from_decimal_string("8543054.951085025").unwrap();
        assert_eq!(
            rebuild_iou(&original, &redrived),
            Some(serde_json::json!("8543055"))
        );
    }

    #[test]
    fn round_drops_ties_to_even() {
        assert_eq!(round_drops_half_even("10.5"), Some(10));
        assert_eq!(round_drops_half_even("11.5"), Some(12));
        assert_eq!(round_drops_half_even("10.5000001"), Some(11));
        assert_eq!(round_drops_half_even("10.4999999"), Some(10));
        assert_eq!(round_drops_half_even("42"), Some(42));
    }
}

#[cfg(test)]
mod book_directory_quality_tests {
    use super::offer_book_quality;

    // Mainnet tx DFF0E4CB (ledger 105333100): an offer selling XRP for
    // 140.55304742187 ZRP partially crosses, leaving TakerGets = 3654430 drops
    // (the original tx amount was 3654519). The book directory quality must be
    // the rate of the PLACED (leftover) amounts, so its low 64 bits are
    // 0x500DAA02090C875D, not the original-amount 0x500DA9EC3A23F7E9.
    #[test]
    fn book_quality_uses_placed_leftover_amounts() {
        let zrp = serde_json::json!({
            "currency": "ZRP",
            "issuer": "rZapJ1PZ297QAEXRGu3SZkAiwXbA7BNoe",
            "value": "140.55304742187"
        });
        let remaining_gets = serde_json::json!("3654430");
        let original_gets = serde_json::json!("3654519");
        assert_eq!(
            offer_book_quality(&zrp, &remaining_gets, true),
            0x500DAA02090C875D
        );
        assert_eq!(
            offer_book_quality(&zrp, &original_gets, true),
            0x500DA9EC3A23F7E9
        );
    }
}

#[cfg(test)]
mod amm_quality_gate_tests {
    use super::num_quality_iou;
    use rxrpl_amount::number::Number;

    // Mainnet tx 062FDE11 (ledger 105333100): offer sells 1061920 USD for
    // 1e12 drops XRP => limit quality (in/out rate) = get_rate(1061920, 1e12).
    // The USD/XRP AMM (4014336154 drops XRP + 4363.056921446038 USD) has a spot
    // quality of only 920074 drops/USD, worse than the offer's 941690, so the
    // gate must refuse the cross (amm_hop returns None) and the offer rests.
    #[test]
    fn amm_worse_than_offer_quality_is_gated_out() {
        // AMM spot rate: in = pool USD (received), out = pool XRP (paid).
        let pool_in = Number::from_iou(
            &rxrpl_amount::IOUAmount::from_decimal_string("4363.056921446038").unwrap(),
        );
        let pool_out = Number::from_int(4_014_336_154); // XRP drops
        let spq = rxrpl_amount::get_rate(
            &num_quality_iou(&pool_in, false),
            &num_quality_iou(&pool_out, true),
        )
        .unwrap();
        // Offer limit: in = TakerGets 1061920 USD, out = TakerPays 1e12 drops.
        let offer_in =
            Number::from_iou(&rxrpl_amount::IOUAmount::from_decimal_string("1061920").unwrap());
        let offer_out = Number::from_int(1_000_000_000_000);
        let cq = rxrpl_amount::get_rate(
            &num_quality_iou(&offer_in, false),
            &num_quality_iou(&offer_out, true),
        )
        .unwrap();
        // Gate refuses when the AMM does not strictly beat the offer quality.
        assert!(
            !rxrpl_amount::is_better_quality(spq, cq)
                || rxrpl_amount::within_relative_distance(spq, cq),
            "AMM spot must not beat the offer limit -> no cross"
        );
    }
}

#[cfg(test)]
mod owner_funds_tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::ApplyContext;
    use crate::view::ledger_view::LedgerView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const MAKER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const ISSUER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    // Build a ledger where MAKER holds 100 USD from ISSUER with the given
    // trust-line and issuer-account flags, then return the maker's offer funds
    // for an offer that SELLS that USD (`owner_funds_leg` on the USD leg).
    fn maker_usd_funds(line_flags: u64, issuer_flags: u64) -> Leg {
        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISSUER).unwrap();
        let mut cur = [0u8; 20];
        cur[12..15].copy_from_slice(b"USD");
        let issuer_is_low = issuer.as_bytes() < maker.as_bytes();
        // Balance is the low account's view; the maker (holder) is positive iff
        // value < 0 when the issuer is low, else value > 0.
        let value = if issuer_is_low { "-100" } else { "100" };
        let (low, high) = if issuer_is_low {
            (ISSUER, MAKER)
        } else {
            (MAKER, ISSUER)
        };
        let mut ledger = Ledger::genesis();
        for (addr, id, flags) in [(MAKER, &maker, 0u64), (ISSUER, &issuer, issuer_flags)] {
            let acct = serde_json::json!({
                "LedgerEntryType": "AccountRoot", "Account": addr,
                "Balance": "100000000", "Sequence": 1, "OwnerCount": 0, "Flags": flags,
            });
            ledger
                .put_state(keylet::account(id), serde_json::to_vec(&acct).unwrap())
                .unwrap();
        }
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": value},
            "LowLimit": {"currency": "USD", "issuer": low, "value": "0"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000000"},
            "Flags": line_flags,
        });
        ledger
            .put_state(
                keylet::trust_line(&maker, &issuer, &cur),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let gets = Leg {
            is_xrp: false,
            drops: 0,
            iou: IOUAmount::ZERO,
            currency: cur,
            issuer,
        };
        owner_funds_leg(&mut ctx, &maker, &gets)
    }

    fn freeze_flag() -> u64 {
        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISSUER).unwrap();
        if issuer.as_bytes() < maker.as_bytes() {
            0x0040_0000 // lsfLowFreeze
        } else {
            0x0080_0000 // lsfHighFreeze
        }
    }

    fn auth_flag() -> u64 {
        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISSUER).unwrap();
        if issuer.as_bytes() < maker.as_bytes() {
            0x0004_0000 // lsfLowAuth
        } else {
            0x0008_0000 // lsfHighAuth
        }
    }

    #[test]
    fn clean_line_is_funded() {
        assert!(!maker_usd_funds(0, 0).is_zero());
    }

    #[test]
    fn issuer_frozen_line_is_zero() {
        assert!(maker_usd_funds(freeze_flag(), 0).is_zero());
    }

    #[test]
    fn issuer_global_freeze_is_zero() {
        assert!(maker_usd_funds(0, 0x0040_0000).is_zero());
    }

    #[test]
    fn require_auth_unauthorized_is_zero() {
        assert!(maker_usd_funds(0, 0x0004_0000).is_zero());
    }

    #[test]
    fn require_auth_authorized_is_funded() {
        assert!(!maker_usd_funds(auth_flag(), 0x0004_0000).is_zero());
    }

    // Does MAKER's USD line report deep-frozen for the given line flags?
    fn maker_usd_deep_frozen(line_flags: u64) -> bool {
        let maker = decode_account_id(MAKER).unwrap();
        let issuer = decode_account_id(ISSUER).unwrap();
        let mut cur = [0u8; 20];
        cur[12..15].copy_from_slice(b"USD");
        let issuer_is_low = issuer.as_bytes() < maker.as_bytes();
        let (low, high) = if issuer_is_low {
            (ISSUER, MAKER)
        } else {
            (MAKER, ISSUER)
        };
        let mut ledger = Ledger::genesis();
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": "USD", "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": "0"},
            "LowLimit": {"currency": "USD", "issuer": low, "value": "0"},
            "HighLimit": {"currency": "USD", "issuer": high, "value": "1000000"},
            "Flags": line_flags,
        });
        ledger
            .put_state(
                keylet::trust_line(&maker, &issuer, &cur),
                serde_json::to_vec(&tl).unwrap(),
            )
            .unwrap();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({});
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let asset = Leg {
            is_xrp: false,
            drops: 0,
            iou: IOUAmount::ZERO,
            currency: cur,
            issuer,
        };
        is_deep_frozen(&mut ctx, &maker, &asset)
    }

    #[test]
    fn deep_freeze_detected_either_side() {
        assert!(maker_usd_deep_frozen(0x0200_0000)); // lsfLowDeepFreeze
        assert!(maker_usd_deep_frozen(0x0400_0000)); // lsfHighDeepFreeze
    }

    #[test]
    fn clean_or_regular_freeze_is_not_deep_frozen() {
        assert!(!maker_usd_deep_frozen(0));
        assert!(!maker_usd_deep_frozen(0x0040_0000)); // regular freeze, not deep
    }
}
