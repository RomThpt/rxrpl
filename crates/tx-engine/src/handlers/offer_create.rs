use rxrpl_amendment::feature::feature_id;
use rxrpl_amount::{IOUAmount, offer_quality};
use rxrpl_codec::address::classic::decode_account_id;
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

        // Owner reserve: a resting offer needs reserve for one more owned
        // object. rippled returns tecINSUF_RESERVE_OFFER — fee and sequence
        // charged, no offer placed — when the account cannot afford it.
        let owner_count = acct.get("OwnerCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if helpers::get_balance(&acct) < ctx.fees.account_reserve(owner_count + 1) {
            let nb = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .update(acct_key, nb)
                .map_err(|_| TransactionResult::TemMalformed)?;
            return Ok(TransactionResult::TecInsufReserveOffer);
        }

        // Sweep unfunded crossing offers from the inverse book. Matching the
        // taker side of this new offer means existing offers where someone
        // pays our `TakerGets` to receive our `TakerPays` — i.e. the book
        // keyed by `(gets, pays)`. Any such offer whose owner can no longer
        // deliver its TakerGets is removed before we place the new offer
        // (rippled's `dirAdvance` skips and deletes funded-failed offers).
        let inverse_book =
            keylet::book_dir(&gets_currency, &gets_issuer, &pays_currency, &pays_issuer);
        sweep_unfunded_offers(ctx, &inverse_book)?;

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
        offer.insert("TakerPays".into(), ctx.tx["TakerPays"].clone());
        offer.insert("TakerGets".into(), ctx.tx["TakerGets"].clone());
        offer.insert("BookDirectory".into(), book_dir_key.to_string().into());
        let flags = ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0);
        if flags != 0 {
            offer.insert("Flags".into(), Value::from(flags));
        }
        if book_node != 0 {
            offer.insert("BookNode".into(), u64_hex(book_node).into());
        }
        if owner_node != 0 {
            offer.insert("OwnerNode".into(), u64_hex(owner_node).into());
        }
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

/// Walk the order-book directory and remove offers whose owner can no longer
/// deliver `TakerGets`. Matches rippled's behavior in `BookOfferCrossing` /
/// `dirAdvance` where exhausted/unfunded offers are reaped before crossing
/// proceeds. Cleanup includes: removing the offer index from the book dir
/// AND from the owner's owner_dir, decrementing the owner's `OwnerCount`,
/// and erasing the SLE.
fn sweep_unfunded_offers(
    ctx: &mut ApplyContext<'_>,
    book_root: &rxrpl_primitives::Hash256,
) -> Result<(), TransactionResult> {
    // Collect candidates first (avoid mutating while iterating dir pages).
    let mut candidates: Vec<rxrpl_primitives::Hash256> = Vec::new();
    let mut page = 0u64;
    loop {
        let page_key = if page == 0 {
            *book_root
        } else {
            keylet::dir_node(book_root, page)
        };
        let page_bytes = match ctx.view.read(&page_key) {
            Some(b) => b,
            None => break,
        };
        let page_json: Value = match serde_json::from_slice(&page_bytes) {
            Ok(v) => v,
            Err(_) => break,
        };
        if let Some(indexes) = page_json.get("Indexes").and_then(|v| v.as_array()) {
            for idx_val in indexes {
                if let Some(s) = idx_val.as_str() {
                    if let Ok(h) = s.parse::<rxrpl_primitives::Hash256>() {
                        candidates.push(h);
                    }
                }
            }
        }
        match page_json.get("IndexNext").and_then(|v| v.as_u64()) {
            Some(next) if next != 0 => page = next,
            _ => break,
        }
    }

    for offer_key in candidates {
        let entry_bytes = match ctx.view.read(&offer_key) {
            Some(b) => b,
            None => continue,
        };
        let entry: Value = match serde_json::from_slice(&entry_bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if entry.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
            continue;
        }
        let owner_str = match entry.get("Account").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };
        let owner_id = match decode_account_id(owner_str) {
            Ok(id) => id,
            Err(_) => continue,
        };
        let gets = entry.get("TakerGets").unwrap_or(&Value::Null);

        let funded = is_offer_funded(ctx, &owner_id, gets);
        if funded {
            continue;
        }

        // Unfunded: reap from book dir, owner dir, decrement owner count,
        // erase the SLE.
        crate::owner_dir::remove_from_owner_dir(ctx.view, &owner_id, &offer_key)?;

        // Remove from the book directory page.
        remove_from_book_dir(ctx.view, book_root, &offer_key)?;

        let _ = ctx.view.erase(&offer_key);
        let owner_acct_key = keylet::account(&owner_id);
        if let Some(b) = ctx.view.read(&owner_acct_key) {
            if let Ok(mut acct) = serde_json::from_slice::<Value>(&b) {
                helpers::adjust_owner_count(&mut acct, -1);
                if let Ok(nb) = serde_json::to_vec(&acct) {
                    let _ = ctx.view.update(owner_acct_key, nb);
                }
            }
        }
    }
    Ok(())
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
    let issuer_is_low = issuer_id.as_bytes() < owner.as_bytes();
    let holder_view = if issuer_is_low { raw } else { -raw };
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
