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

/// Reserialize an IOU offer side with a recomputed magnitude, preserving its
/// currency/issuer. Returns `None` for XRP sides (no tick rounding applies to
/// a recomputed native amount here).
fn rebuild_iou(original: &Value, value: &IOUAmount) -> Option<Value> {
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
        let number_switchover = ctx.rules.enabled(&feature_id("fixUniversalNumber"));
        let quality = offer_book_quality(&taker_pays, &taker_gets, number_switchover);
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
    'walk: while let Some(dir_key) = ctx.view.succ(&probe) {
        if &dir_key.as_bytes()[0..24] != book_prefix {
            break; // left this book
        }
        probe = dir_key;
        let dir_quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
        if dir_quality > threshold {
            break; // worse than the taker will accept
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
            if remaining_out.is_zero() {
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
            // Owner-funds clamp: an offer can give at most what its owner
            // holds. Fully funded → the whole offer is available; underfunded
            // but positive → fill against the funded amount; zero → reap.
            let funds = owner_funds_leg(ctx, &owner, &offer_out);
            if funds.is_zero() {
                reap_offer(ctx, &owner, &offer_key, &dir_key)?;
                continue;
            }
            let avail_out = leg_min(&offer_out, &funds);
            let take_out = leg_min(&remaining_out, &avail_out);

            // Full take when the taker consumes the whole original offer;
            // otherwise a partial take of `take_out`, priced at the offer's
            // quality (`order_in = take_out * rate`, clamped to the offer).
            let full_take = leg_ge(&take_out, &offer_out);
            let (order_out, order_in) = if full_take {
                (offer_out.clone(), offer_in.clone())
            } else {
                let rate = rxrpl_amount::from_rate(dir_quality).unwrap_or(IOUAmount::ZERO);
                let computed = rxrpl_amount::Amount::multiply(
                    &leg_to_amount(&take_out),
                    &rxrpl_amount::Amount::Iou(rate),
                    in_leg.is_xrp,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
                let order_in = leg_min(&amount_to_leg(&computed, &offer_in), &offer_in);
                (take_out.clone(), order_in)
            };

            // Move funds: taker pays order_in (grossed), owner pays order_out.
            // Legacy Taker semantics: `order_out` is the NET the taker receives;
            // the owner's debit is grossed up by the output issuer's transfer
            // fee inside `pay_out`.
            pay_in(ctx, taker, taker_acct, &owner, &order_in, false, false)?;
            pay_out(ctx, taker, taker_acct, &owner, taker, &order_out, false)?;

            if full_take {
                // Consume to zero (records the change in metadata) then delete,
                // as rippled consumes before BookTip drops the offer.
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
                // Reduce the resting offer in place by the filled amounts.
                let new_gets = leg_sub(&offer_out, &order_out);
                let new_pays = leg_sub(&offer_in, &order_in);
                let mut reduced = offer.clone();
                reduced["TakerGets"] = new_gets.with_amount(&new_gets.iou, new_gets.drops);
                reduced["TakerPays"] = new_pays.with_amount(&new_pays.iou, new_pays.drops);
                let rb =
                    serde_json::to_vec(&reduced).map_err(|_| TransactionResult::TefInternal)?;
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

/// Output deliverable for a given input at a resting offer's price. This mirrors
/// rippled's `Quality::ceilInStrict` (the input-limited path in `BookStep`):
/// the offer quality is first *quantized* into a rate `q = offer_in / offer_out`
/// (`getRate`'s 16-digit `divide`), then `out = in / q` rounded DOWN
/// (`divRoundStrict`). Pricing from the bucketed rate rather than the
/// full-precision `in * offer_out / offer_in` is what makes the consumed/owner
/// amounts byte-exact (e.g. mainnet SOLO fills land on `…2747`, not `…2750`).
/// Magnitudes are pure `IOUAmount` (drops count as integers) to avoid the
/// native/IOU normalisation hazard of mixed-asset multiply.
fn out_for_in(in_amt: &Leg, offer_in: &Leg, offer_out: &Leg) -> Leg {
    let in_iou = leg_as_quality_iou(in_amt);
    let offer_out_iou = leg_as_quality_iou(offer_out);
    let offer_in_iou = leg_as_quality_iou(offer_in);
    let qrate = IOUAmount::divide(&offer_in_iou, &offer_out_iou).unwrap_or(IOUAmount::ZERO);
    let out_iou =
        IOUAmount::div_round(&in_iou, &qrate, /*round_up*/ false).unwrap_or(IOUAmount::ZERO);
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
        out.drops = whole.parse::<i64>().unwrap_or(0);
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
) -> Result<(Leg, Leg), TransactionResult> {
    let inverse_book = keylet::book_dir(
        &budget_in.currency,
        &budget_in.issuer,
        &demand_out.currency,
        &demand_out.issuer,
    );

    let out_start = demand_out.clone();
    let budget_start = budget_in.clone();
    let mut remaining_out = demand_out.clone();
    let mut remaining_in = budget_in.clone();
    let book_prefix = inverse_book.as_bytes()[0..24].to_vec();
    let mut probe = book_dir_with_quality(&inverse_book, 0);
    'walk: while let Some(dir_key) = ctx.view.succ(&probe) {
        if dir_key.as_bytes()[0..24] != book_prefix[..] {
            break;
        }
        probe = dir_key;
        let dir_quality = u64::from_be_bytes(dir_key.as_bytes()[24..32].try_into().unwrap());
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

            // Output capped by remaining demand, funded availability, and what
            // the remaining input budget can buy at this offer's price.
            let budget_out = out_for_in(&remaining_in, &offer_in, &offer_out);
            let mut take_out = leg_min(&remaining_out, &avail_out);
            let budget_binds = leg_ge(&take_out, &budget_out);
            if budget_binds {
                take_out = budget_out;
            }

            let full_take = leg_ge(&take_out, &offer_out);
            let (order_out, order_in) = if full_take {
                (offer_out.clone(), offer_in.clone())
            } else if budget_binds {
                // Input-limited: spend the whole remaining budget, deliver floor.
                (take_out.clone(), remaining_in.clone())
            } else {
                // Demand-limited: pay the ceil price for the delivered output,
                // never exceeding the resting offer or the remaining budget.
                let priced = in_for_out(&take_out, &rate, &offer_in);
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
        }
    }

    let delivered = leg_sub(&out_start, &remaining_out);
    let spent = leg_sub(&budget_start, &remaining_in);
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
            /*skip_output_credit=*/ !is_last,
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
        carry = got.clone();
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
        // rippled's STAmount max mantissa/exponent — far above any book size.
        out.iou =
            IOUAmount::from_parts(9_999_999_999_999_999, 80, false).unwrap_or(IOUAmount::ZERO);
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
fn amm_hop(
    ctx: &mut ApplyContext<'_>,
    taker: &AccountId,
    taker_acct: &mut Value,
    dest: &AccountId,
    demand_out: &Leg,
    budget_in: &Leg,
    skip_input_debit: bool,
    skip_output_credit: bool,
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

    // Output-limited (deliver the demand) when the input required fits the
    // budget; otherwise input-limited (spend the whole budget).
    let out_full =
        crate::amm_helpers::swap_asset_in(&pool_in, &pool_out, &budget_num, tfee, out_xrp);
    let (spent_num, deliver_num) = if num_le(&out_full, &demand_num) {
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
    credit_line(
        ctx,
        owner,
        &amount.issuer,
        &amount.currency,
        &gross.negate(),
        round,
    )?;
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
    credit_line(
        ctx,
        owner,
        &amount.issuer,
        &amount.currency,
        &amount.iou.negate(),
        round,
    )?;
    if !skip_recipient_credit {
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
        let bal = ctx
            .view
            .read(&key)
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|a| {
                a.get("Balance")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
            })
            .unwrap_or(0);
        if bal <= 0 {
            return zero;
        }
        return Leg {
            is_xrp: true,
            drops: bal,
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
