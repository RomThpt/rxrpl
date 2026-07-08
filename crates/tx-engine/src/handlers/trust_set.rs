use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_primitives::AccountId;
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::helpers;
use crate::owner_dir::add_to_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

// RippleState (trust line) flags.
const LSF_LOW_RESERVE: u32 = 0x0001_0000;
const LSF_HIGH_RESERVE: u32 = 0x0002_0000;
const LSF_LOW_AUTH: u32 = 0x0004_0000;
const LSF_HIGH_AUTH: u32 = 0x0008_0000;
const LSF_LOW_NO_RIPPLE: u32 = 0x0010_0000;
const LSF_HIGH_NO_RIPPLE: u32 = 0x0020_0000;
const LSF_LOW_FREEZE: u32 = 0x0040_0000;
const LSF_HIGH_FREEZE: u32 = 0x0080_0000;
const LSF_LOW_DEEP_FREEZE: u32 = 0x0200_0000;
const LSF_HIGH_DEEP_FREEZE: u32 = 0x0400_0000;
// AccountRoot flags.
const LSF_DEFAULT_RIPPLE: u32 = 0x0080_0000;
const LSF_NO_FREEZE: u32 = 0x0020_0000;
// TrustSet transaction flags (rippled TxFlags.h).
const TF_SETF_AUTH: u32 = 0x0001_0000;
const TF_SET_NO_RIPPLE: u32 = 0x0002_0000;
const TF_CLEAR_NO_RIPPLE: u32 = 0x0004_0000;
const TF_SET_FREEZE: u32 = 0x0010_0000;
const TF_CLEAR_FREEZE: u32 = 0x0020_0000;
const TF_SET_DEEP_FREEZE: u32 = 0x0040_0000;
const TF_CLEAR_DEEP_FREEZE: u32 = 0x0080_0000;
// Quality value that rippled treats as "unset" (1.0).
const QUALITY_ONE: u32 = 1_000_000_000;

// The DefaultRipple amendment predates rippled's `sha512Half(name)` feature-ID
// convention, so its on-ledger amendment ID is a hardcoded value that does NOT
// equal `feature_id("DefaultRipple")`. Use the real ID so `Rules::enabled`
// resolves correctly on mainnet (where DefaultRipple has been active since 2015).
const DEFAULT_RIPPLE_AMENDMENT_ID: [u8; 32] = [
    0x15, 0x62, 0x51, 0x1F, 0x57, 0x3A, 0x19, 0xAE, 0x9B, 0xD1, 0x03, 0xB5, 0xD6, 0xB9, 0xE0, 0x1B,
    0x3B, 0x46, 0x80, 0x5A, 0xEC, 0x5D, 0x3C, 0x48, 0x05, 0xC9, 0x02, 0xB5, 0x14, 0x39, 0x91, 0x46,
];

/// rippled `trustDelete` fired from `rippleCredit`/`directSendNoFeeIOU`: after an
/// IOU credit has rewritten a RippleState balance, delete the line if it has
/// fallen to the default state with a zero balance. No-op when the line is
/// absent, non-default (non-zero limit/quality/freeze/noRipple-override on either
/// side), or still carries a non-zero balance. Only the sender (the reserve
/// holder whose balance drained to zero) has its OwnerCount decremented, in the
/// caller's `sender_acct` working copy; the issuer AccountRoot is re-threaded.
pub(crate) fn maybe_delete_drained_trust_line(
    ctx: &mut ApplyContext<'_>,
    sender_id: &AccountId,
    sender_acct: &mut Value,
    issuer_id: &AccountId,
    currency: &[u8; 20],
) -> Result<bool, TransactionResult> {
    let tl_key = keylet::trust_line(sender_id, issuer_id, currency);
    let Some(bytes) = ctx.view.read(&tl_key) else {
        return Ok(false);
    };
    let obj: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;

    let default_ripple = ctx.rules.enabled(&rxrpl_primitives::Hash256::from(
        DEFAULT_RIPPLE_AMENDMENT_ID,
    ));

    let is_low = sender_id.as_bytes() < issuer_id.as_bytes();
    let low_id: &AccountId = if is_low { sender_id } else { issuer_id };
    let high_id: &AccountId = if is_low { issuer_id } else { sender_id };

    let issuer_key = keylet::account(issuer_id);
    let issuer_acct = ctx
        .view
        .read(&issuer_key)
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());

    let amt = |o: &Value, field: &str| -> f64 {
        o.get(field)
            .and_then(|a| a.get("value"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0)
    };
    let qual = |o: &Value, field: &str| -> u32 {
        let q = o.get(field).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if q == QUALITY_ONE { 0 } else { q }
    };
    let def_ripple = |a: &Value| -> bool {
        (a.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) as u32 & LSF_DEFAULT_RIPPLE) != 0
    };

    let flags = obj.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let low_balance = amt(&obj, "Balance");
    let high_balance = -low_balance;

    let low_def_ripple = if is_low {
        def_ripple(sender_acct)
    } else {
        issuer_acct.as_ref().map(def_ripple).unwrap_or(false)
    };
    let high_def_ripple = if is_low {
        issuer_acct.as_ref().map(def_ripple).unwrap_or(false)
    } else {
        def_ripple(sender_acct)
    };

    let low_no_ripple_reserve = if default_ripple {
        ((flags & LSF_LOW_NO_RIPPLE) == 0) != low_def_ripple
    } else {
        (flags & LSF_LOW_NO_RIPPLE) != 0
    };
    let high_no_ripple_reserve = if default_ripple {
        ((flags & LSF_HIGH_NO_RIPPLE) == 0) != high_def_ripple
    } else {
        (flags & LSF_HIGH_NO_RIPPLE) != 0
    };

    let low_reserve_set = qual(&obj, "LowQualityIn") != 0
        || qual(&obj, "LowQualityOut") != 0
        || low_no_ripple_reserve
        || (flags & LSF_LOW_FREEZE) != 0
        || amt(&obj, "LowLimit") != 0.0
        || low_balance > 0.0;
    let high_reserve_set = qual(&obj, "HighQualityIn") != 0
        || qual(&obj, "HighQualityOut") != 0
        || high_no_ripple_reserve
        || (flags & LSF_HIGH_FREEZE) != 0
        || amt(&obj, "HighLimit") != 0.0
        || high_balance > 0.0;

    if low_reserve_set || high_reserve_set || low_balance != 0.0 {
        return Ok(false);
    }

    let sender_reserve_flag = if is_low { LSF_LOW_RESERVE } else { LSF_HIGH_RESERVE };
    if (flags & sender_reserve_flag) != 0 {
        helpers::adjust_owner_count(sender_acct, -1);
    }

    let parse_node = |o: &Value, f: &str| -> u64 {
        o.get(f)
            .and_then(|v| v.as_str())
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0)
    };
    let low_node = parse_node(&obj, "LowNode");
    let high_node = parse_node(&obj, "HighNode");
    crate::owner_dir::remove_from_owner_dir_page(ctx.view, low_id, low_node, &tl_key)?;
    crate::owner_dir::remove_from_owner_dir_page(ctx.view, high_id, high_node, &tl_key)?;
    let _ = ctx.view.erase(&tl_key);

    // rippled threadOwners re-threads BOTH owner roots on delete. The sender's
    // root is the caller's working copy (written back → threaded); the issuer's
    // root is re-written unchanged here so central stamping bumps its
    // PreviousTxnID into a threading-only ModifiedNode.
    if let Some(a) = issuer_acct {
        if let Ok(nb) = serde_json::to_vec(&a) {
            let _ = ctx.view.update(issuer_key, nb);
        }
    }

    Ok(true)
}

/// rippled `computeFreezeFlags`: apply set/clear freeze and deep-freeze to a
/// side's flags. `bNoFreeze` (the account's lsfNoFreeze) blocks setting freeze.
fn compute_freeze_flags(
    mut flags: u32,
    high: bool,
    no_freeze: bool,
    set_freeze: bool,
    clear_freeze: bool,
    set_deep_freeze: bool,
    clear_deep_freeze: bool,
) -> u32 {
    let (freeze, deep_freeze) = if high {
        (LSF_HIGH_FREEZE, LSF_HIGH_DEEP_FREEZE)
    } else {
        (LSF_LOW_FREEZE, LSF_LOW_DEEP_FREEZE)
    };
    if set_freeze && !clear_freeze && !no_freeze {
        flags |= freeze;
    } else if clear_freeze && !set_freeze {
        flags &= !freeze;
    }
    if set_deep_freeze && !clear_deep_freeze && !no_freeze {
        flags |= deep_freeze;
    } else if clear_deep_freeze && !set_deep_freeze {
        flags &= !deep_freeze;
    }
    flags
}

/// TrustSet transaction handler.
///
/// Creates or modifies a trust line between two accounts.
pub struct TrustSetTransactor;

impl Transactor for TrustSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let limit = ctx
            .tx
            .get("LimitAmount")
            .ok_or(TransactionResult::TemBadAmount)?;

        // LimitAmount must be an IOU (object with currency/issuer)
        if !limit.is_object() {
            return Err(TransactionResult::TemBadAmount);
        }

        // Must have currency and issuer
        if limit.get("currency").is_none() || limit.get("issuer").is_none() {
            return Err(TransactionResult::TemBadCurrency);
        }

        // Cannot trust self
        let account = helpers::get_account(ctx.tx)?;
        if let Some(issuer) = limit.get("issuer").and_then(|v| v.as_str()) {
            if account == issuer {
                return Err(TransactionResult::TemDstIsObligatory);
            }
        }

        // Negative limit is invalid (rippled returns temBAD_LIMIT).
        let limit_value = limit.get("value").and_then(|v| v.as_str()).unwrap_or("0");
        if limit_value.starts_with('-') {
            return Err(TransactionResult::TemBadLimit);
        }

        // Zero limit with non-zero QualityIn/QualityOut is malformed (rippled
        // doesn't allow setting quality on a zero trust line).
        let limit_zero = limit_value == "0"
            || limit_value
                .parse::<f64>()
                .map(|f| f == 0.0)
                .unwrap_or(false);
        if limit_zero {
            let qi = helpers::get_u32_field(ctx.tx, "QualityIn").unwrap_or(0);
            let qo = helpers::get_u32_field(ctx.tx, "QualityOut").unwrap_or(0);
            if qi != 0 || qo != 0 {
                return Err(TransactionResult::TemMalformed);
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

        // Check issuer account exists
        let limit = ctx.tx.get("LimitAmount").unwrap();
        let issuer_str = limit["issuer"]
            .as_str()
            .ok_or(TransactionResult::TemBadIssuer)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemBadIssuer)?;
        let issuer_key = keylet::account(&issuer_id);
        let issuer_bytes = ctx
            .view
            .read(&issuer_key)
            .ok_or(TransactionResult::TecNoDst)?;

        // DisallowIncomingTrustline: if issuer set asfDisallowIncomingTrustline
        // (lsfDisallowIncomingTrustline = 0x40000000), reject TrustSet from a
        // different account. Holder must already trust BEFORE issuer turns the
        // flag on; new incoming trust lines are blocked.
        let issuer_account: serde_json::Value =
            serde_json::from_slice(&issuer_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let issuer_flags = issuer_account
            .get("Flags")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        const LSF_DISALLOW_INCOMING_TRUSTLINE: u32 = 0x20000000;
        if issuer_flags & LSF_DISALLOW_INCOMING_TRUSTLINE != 0 && account_str != issuer_str {
            // Check if a trust line already exists between these two accounts;
            // updating an existing line is allowed.
            let existing = keylet::trust_line(
                &account_id,
                &issuer_id,
                &currency_to_bytes(limit["currency"].as_str().unwrap_or("")),
            );
            if !ctx.view.exists(&existing) {
                return Err(TransactionResult::TecNoPermission);
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;

        let limit = ctx.tx.get("LimitAmount").unwrap();
        let issuer_str = limit["issuer"]
            .as_str()
            .ok_or(TransactionResult::TemBadIssuer)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemBadIssuer)?;

        let currency_bytes = currency_to_bytes(
            limit["currency"]
                .as_str()
                .ok_or(TransactionResult::TemBadCurrency)?,
        );

        // Compute trust line keylet (symmetric between the two accounts)
        let tl_key = keylet::trust_line(&account_id, &issuer_id, &currency_bytes);

        // Transaction flags (which side / what to set is decided by `is_low`).
        let tx_flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let b_set_auth = tx_flags & TF_SETF_AUTH != 0;
        let b_set_no_ripple = tx_flags & TF_SET_NO_RIPPLE != 0;
        let b_clear_no_ripple = tx_flags & TF_CLEAR_NO_RIPPLE != 0;
        let b_set_freeze = tx_flags & TF_SET_FREEZE != 0;
        let b_clear_freeze = tx_flags & TF_CLEAR_FREEZE != 0;
        let b_set_deep_freeze = tx_flags & TF_SET_DEEP_FREEZE != 0;
        let b_clear_deep_freeze = tx_flags & TF_CLEAR_DEEP_FREEZE != 0;

        // QualityIn / QualityOut from the tx; QualityOut==QUALITY_ONE normalizes
        // to 0 (clear), matching rippled. QualityIn is NOT normalized before the
        // set/clear decision (rippled quirk).
        let b_quality_in = ctx.tx.get("QualityIn").is_some();
        let b_quality_out = ctx.tx.get("QualityOut").is_some();
        let u_quality_in = helpers::get_u32_field(ctx.tx, "QualityIn").unwrap_or(0);
        let mut u_quality_out = helpers::get_u32_field(ctx.tx, "QualityOut").unwrap_or(0);
        if b_quality_out && u_quality_out == QUALITY_ONE {
            u_quality_out = 0;
        }

        // DefaultRipple amendment state (uses the real on-ledger amendment ID).
        let default_ripple = ctx.rules.enabled(&rxrpl_primitives::Hash256::from(
            DEFAULT_RIPPLE_AMENDMENT_ID,
        ));

        // Check if trust line exists
        let existing = ctx.view.read(&tl_key);

        if let Some(bytes) = existing {
            // Update existing trust line
            let obj_orig: Value =
                serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;
            let mut obj = obj_orig.clone();

            // Determine which side we are (low or high). The `*Limit.issuer`
            // field is THIS side's address (rippled convention), so rebuild
            // the limit object with the sender's address rather than copying
            // the tx's `LimitAmount` (whose `issuer` is the counterparty).
            let is_low = account_id.as_bytes() < issuer_id.as_bytes();
            let limit_value = limit.get("value").cloned().unwrap_or_else(|| "0".into());
            let side_limit = serde_json::json!({
                "currency": limit["currency"],
                "issuer": account_str,
                "value": limit_value,
            });
            if is_low {
                obj["LowLimit"] = side_limit;
            } else {
                obj["HighLimit"] = side_limit;
            }

            // Apply quality settings: set when present and non-zero, clear
            // (remove the field) when present and zero, keep otherwise. rippled
            // mutates only this side's QualityIn/QualityOut.
            let set_or_clear = |obj: &mut Value, field: &str, present: bool, value: u32| {
                if !present {
                    return;
                }
                if value != 0 {
                    obj[field] = Value::from(value);
                } else if let Some(m) = obj.as_object_mut() {
                    m.remove(field);
                }
            };
            let (qin_field, qout_field) = if is_low {
                ("LowQualityIn", "LowQualityOut")
            } else {
                ("HighQualityIn", "HighQualityOut")
            };
            set_or_clear(&mut obj, qin_field, b_quality_in, u_quality_in);
            set_or_clear(&mut obj, qout_field, b_quality_out, u_quality_out);

            // Reserve accounting (rippled SetTrust::doApply): a side counts
            // toward its account's owner reserve when its trust line is not in
            // the default state — non-zero limit, balance owed to it, quality
            // set, freeze, or a noRipple flag that disagrees with the account's
            // DefaultRipple. Flipping that state sets/clears lsfLow/HighReserve
            // and adjusts the owning account's OwnerCount.
            let amt = |o: &Value, field: &str| -> f64 {
                o.get(field)
                    .and_then(|a| a.get("value"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0)
            };
            let qual = |o: &Value, field: &str| -> u32 {
                let q = o.get(field).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                if q == QUALITY_ONE { 0 } else { q }
            };
            let low_id = if is_low { account_id } else { issuer_id };
            let high_id = if is_low { issuer_id } else { account_id };
            let low_key = keylet::account(&low_id);
            let high_key = keylet::account(&high_id);
            let mut low_acct = ctx
                .view
                .read(&low_key)
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
            let mut high_acct = ctx
                .view
                .read(&high_key)
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
            let def_ripple = |a: &Option<Value>| -> bool {
                a.as_ref()
                    .map(|a| {
                        (a.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) as u32
                            & LSF_DEFAULT_RIPPLE)
                            != 0
                    })
                    .unwrap_or(false)
            };
            let flags_in = obj.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let low_balance = amt(&obj, "Balance");
            let high_balance = -low_balance;

            // Compute the post-tx flags (rippled `uFlagsOut`): apply the tx's
            // noRipple / freeze / auth changes to THIS side BEFORE deriving the
            // reserve terms, exactly as rippled does.
            let mut flags_out = flags_in;
            let side_no_ripple = if is_low {
                LSF_LOW_NO_RIPPLE
            } else {
                LSF_HIGH_NO_RIPPLE
            };
            if b_set_no_ripple && !b_clear_no_ripple {
                let sender_balance = if is_low { low_balance } else { high_balance };
                if sender_balance >= 0.0 {
                    flags_out |= side_no_ripple;
                } else {
                    // Cannot set noRipple on a negative balance.
                    return Err(TransactionResult::TecNoPermission);
                }
            } else if b_clear_no_ripple && !b_set_no_ripple {
                flags_out &= !side_no_ripple;
            }
            let b_high = !is_low;
            // lsfNoFreeze lives on the SENDER's account root.
            let sender_no_freeze = {
                let sender = if is_low {
                    low_acct.as_ref()
                } else {
                    high_acct.as_ref()
                };
                sender
                    .map(|a| helpers::get_flags(a) & LSF_NO_FREEZE != 0)
                    .unwrap_or(false)
            };
            flags_out = compute_freeze_flags(
                flags_out,
                b_high,
                sender_no_freeze,
                b_set_freeze,
                b_clear_freeze,
                b_set_deep_freeze,
                b_clear_deep_freeze,
            );
            if b_set_auth {
                flags_out |= if is_low { LSF_LOW_AUTH } else { LSF_HIGH_AUTH };
            }

            // The noRipple term that contributes to reserve changed with the
            // DefaultRipple amendment (2015). Before it, a side counts a noRipple
            // line toward reserve when its line flag is set; after it, when the
            // line's noRipple state disagrees with the account's DefaultRipple.
            // Replaying pre-amendment (2013) ledgers needs the historical form.
            let low_no_ripple_reserve = if default_ripple {
                ((flags_out & LSF_LOW_NO_RIPPLE) == 0) != def_ripple(&low_acct)
            } else {
                (flags_out & LSF_LOW_NO_RIPPLE) != 0
            };
            let high_no_ripple_reserve = if default_ripple {
                ((flags_out & LSF_HIGH_NO_RIPPLE) == 0) != def_ripple(&high_acct)
            } else {
                (flags_out & LSF_HIGH_NO_RIPPLE) != 0
            };
            let low_reserve_set = qual(&obj, "LowQualityIn") != 0
                || qual(&obj, "LowQualityOut") != 0
                || low_no_ripple_reserve
                || (flags_out & LSF_LOW_FREEZE) != 0
                || amt(&obj, "LowLimit") != 0.0
                || low_balance > 0.0;
            let high_reserve_set = qual(&obj, "HighQualityIn") != 0
                || qual(&obj, "HighQualityOut") != 0
                || high_no_ripple_reserve
                || (flags_out & LSF_HIGH_FREEZE) != 0
                || amt(&obj, "HighLimit") != 0.0
                || high_balance > 0.0;

            // Only accounts actually mutated here are written back (and thus
            // get their PreviousTxnID restamped) — rippled leaves the
            // counterparty untouched when its reserve state does not change.
            let mut low_dirty = false;
            let mut high_dirty = false;
            if low_reserve_set && (flags_in & LSF_LOW_RESERVE) == 0 {
                flags_out |= LSF_LOW_RESERVE;
                if let Some(a) = low_acct.as_mut() {
                    helpers::adjust_owner_count(a, 1);
                    low_dirty = true;
                }
            } else if !low_reserve_set && (flags_in & LSF_LOW_RESERVE) != 0 {
                flags_out &= !LSF_LOW_RESERVE;
                if let Some(a) = low_acct.as_mut() {
                    helpers::adjust_owner_count(a, -1);
                    low_dirty = true;
                }
            }
            if high_reserve_set && (flags_in & LSF_HIGH_RESERVE) == 0 {
                flags_out |= LSF_HIGH_RESERVE;
                if let Some(a) = high_acct.as_mut() {
                    helpers::adjust_owner_count(a, 1);
                    high_dirty = true;
                }
            } else if !high_reserve_set && (flags_in & LSF_HIGH_RESERVE) != 0 {
                flags_out &= !LSF_HIGH_RESERVE;
                if let Some(a) = high_acct.as_mut() {
                    helpers::adjust_owner_count(a, -1);
                    high_dirty = true;
                }
            }
            obj["Flags"] = serde_json::json!(flags_out);

            // The sender's Sequence/Ticket is consumed centrally by the engine
            // (parent sandbox) before doApply; the sender's account (low or high)
            // inherits that bump and is written back via the dirty path below.
            if is_low {
                low_dirty = true;
            } else {
                high_dirty = true;
            }

            // rippled deletes a trust line that ends in the default state with a
            // zero balance (trustDelete); otherwise it persists.
            let b_default = !low_reserve_set && !high_reserve_set;
            if b_default && low_balance == 0.0 {
                // Use the line's LowNode / HighNode page hints to remove the
                // entry directly from each owner directory, exactly like
                // rippled's trustDelete (`dirRemove(ownerDir, uLow/HighNode, …)`).
                // Walking from the root would dead-end when the entry sits in a
                // non-root page whose predecessor pages are not loaded.
                let parse_node = |o: &Value, f: &str| -> u64 {
                    o.get(f)
                        .and_then(|v| v.as_str())
                        .and_then(|s| u64::from_str_radix(s, 16).ok())
                        .unwrap_or(0)
                };
                let low_node = parse_node(&obj, "LowNode");
                let high_node = parse_node(&obj, "HighNode");
                crate::owner_dir::remove_from_owner_dir_page(ctx.view, &low_id, low_node, &tl_key)?;
                crate::owner_dir::remove_from_owner_dir_page(
                    ctx.view, &high_id, high_node, &tl_key,
                )?;
                let _ = ctx.view.erase(&tl_key);
                // Deleting the line removes it from BOTH accounts' owner
                // directories, so rippled re-threads both account roots (updates
                // their PreviousTxnID) even when the side's reserve/owner count
                // is unchanged. Mark both dirty so central stamping threads them.
                low_dirty = true;
                high_dirty = true;
            } else if obj != obj_orig {
                // rippled drops a modified node whose serialized SLE equals the
                // original (ApplyStateTable `*curNode == *origNode`): a no-op
                // TrustSet (re-set an already-set flag, same limit) must not
                // restamp the line's PreviousTxnID nor emit a ModifiedNode.
                let new_bytes =
                    serde_json::to_vec(&obj).map_err(|_| TransactionResult::TemMalformed)?;
                ctx.view
                    .update(tl_key, new_bytes)
                    .map_err(|_| TransactionResult::TemMalformed)?;
            }

            for (key, acct, dirty) in [
                (low_key, low_acct, low_dirty),
                (high_key, high_acct, high_dirty),
            ] {
                if dirty {
                    if let Some(a) = acct {
                        if let Ok(nb) = serde_json::to_vec(&a) {
                            let _ = ctx.view.update(key, nb);
                        }
                    }
                }
            }
        } else {
            // Create new trust line (rippled `trustCreate`).
            //
            // RippleState convention (matches rippled): each side's `*Limit`
            // object carries that side's address as `issuer`, not the
            // counterparty's. `Balance.issuer` is the low account by
            // convention; sign of the balance encodes which side owes whom.
            let is_low = account_id.as_bytes() < issuer_id.as_bytes();
            let acct_value = limit.get("value").cloned().unwrap_or_else(|| "0".into());
            let limit_is_zero = acct_value
                .as_str()
                .map(|s| s == "0" || s.parse::<f64>().map(|f| f == 0.0).unwrap_or(false))
                .unwrap_or(false);

            // rippled: setting a non-existent line to its default state (zero
            // limit, no quality, no auth) is a no-op (tecNO_LINE_REDUNDANT).
            if limit_is_zero
                && (!b_quality_in || u_quality_in == 0)
                && (!b_quality_out || u_quality_out == 0)
                && !b_set_auth
            {
                return Err(TransactionResult::TecNoLineRedundant);
            }

            // Owner reserve (rippled SetTrust::doApply): a brand-new trust line
            // adds one owned object to the creator, so it must fund the reserve.
            // rippled computes `reserveCreate = (OwnerCount < 2) ? 0 :
            // accountReserve(OwnerCount + 1)` — the first two trust lines are
            // reserve-free, a deliberate gateway-funding feature — and returns
            // tecNO_LINE_INSUF_RESERVE when `mPriorBalance` (the XRP balance
            // *before* the fee) is below it. This is a CLAIMED tec (fee and
            // sequence charged, no line created); returning it from apply routes
            // through the engine's central fee/sequence consume. The engine
            // already deducted the fee centrally before doApply, so reconstruct
            // mPriorBalance by adding it back. The check sits only on the create
            // path (after the redundant guard), exactly the
            // `else if (mPriorBalance < reserveCreate)` branch that precedes
            // trustCreate — an existing line takes the update path with no check.
            let creator = ctx
                .view
                .read(&keylet::account(&account_id))
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .ok_or(TransactionResult::TefInternal)?;
            let owner_count = helpers::get_owner_count(&creator);
            let reserve_create = if owner_count < 2 {
                0
            } else {
                ctx.fees.account_reserve(owner_count + 1)
            };
            let prior_balance =
                helpers::get_balance(&creator).saturating_add(helpers::get_fee(ctx.tx));
            if prior_balance < reserve_create {
                return Err(TransactionResult::TecNoLineInsuf);
            }

            let currency = limit["currency"].clone();
            let acct_limit = serde_json::json!({
                "currency": currency,
                "issuer": account_str,
                "value": acct_value,
            });
            let peer_limit = serde_json::json!({
                "currency": currency,
                "issuer": issuer_str,
                "value": "0",
            });
            let (low_limit, high_limit) = if is_low {
                (acct_limit, peer_limit)
            } else {
                (peer_limit, acct_limit)
            };

            // Link the new RippleState into BOTH parties' owner directories
            // (rippled's `trustCreate` inserts into lowDir and highDir),
            // capturing the page each landed in for Low/HighNode.
            let acct_page = add_to_owner_dir(ctx.view, &account_id, &tl_key)?;
            let issuer_page = add_to_owner_dir(ctx.view, &issuer_id, &tl_key)?;
            let (low_node, high_node) = if is_low {
                (acct_page, issuer_page)
            } else {
                (issuer_page, acct_page)
            };

            // Flags (rippled `trustCreate`): the creator always takes the reserve
            // on its side (the redundant guard above filtered out default lines),
            // plus auth / noRipple / freeze it requested. The PEER's side gets
            // noRipple set when the peer's account does not default to rippling.
            let mut flags = if is_low {
                LSF_LOW_RESERVE
            } else {
                LSF_HIGH_RESERVE
            };
            if b_set_auth {
                flags |= if is_low { LSF_LOW_AUTH } else { LSF_HIGH_AUTH };
            }
            if b_set_no_ripple && !b_clear_no_ripple {
                flags |= if is_low {
                    LSF_LOW_NO_RIPPLE
                } else {
                    LSF_HIGH_NO_RIPPLE
                };
            }
            if b_set_freeze && !b_clear_freeze {
                flags |= if is_low {
                    LSF_LOW_FREEZE
                } else {
                    LSF_HIGH_FREEZE
                };
            }
            if b_set_deep_freeze {
                flags |= if is_low {
                    LSF_LOW_DEEP_FREEZE
                } else {
                    LSF_HIGH_DEEP_FREEZE
                };
            }
            // Peer (issuer) side: noRipple if the issuer account does not have
            // lsfDefaultRipple set.
            let issuer_has_default_ripple = ctx
                .view
                .read(&keylet::account(&issuer_id))
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .map(|a| helpers::get_flags(&a) & LSF_DEFAULT_RIPPLE != 0)
                .unwrap_or(false);
            if !issuer_has_default_ripple {
                flags |= if is_low {
                    LSF_HIGH_NO_RIPPLE
                } else {
                    LSF_LOW_NO_RIPPLE
                };
            }

            // RippleState Balance is an STAmount whose issuer is rippled's
            // ACCOUNT_ONE placeholder (20 bytes ending in 1, "rrrr…BZbvji"), not
            // ACCOUNT_ZERO; the sign of the balance encodes which side owes whom.
            let mut account_one = [0u8; 20];
            account_one[19] = 1;
            let no_account = encode_account_id(&AccountId::from(account_one));
            let mut tl_map = serde_json::Map::new();
            tl_map.insert("LedgerEntryType".into(), "RippleState".into());
            tl_map.insert(
                "Balance".into(),
                serde_json::json!({ "currency": currency, "issuer": no_account, "value": "0" }),
            );
            tl_map.insert("LowLimit".into(), low_limit);
            tl_map.insert("HighLimit".into(), high_limit);
            tl_map.insert("LowNode".into(), format!("{low_node:016X}").into());
            tl_map.insert("HighNode".into(), format!("{high_node:016X}").into());
            tl_map.insert("Flags".into(), serde_json::json!(flags));
            // Quality on the creator's side (only when non-zero).
            if b_quality_in && u_quality_in != 0 {
                let f = if is_low {
                    "LowQualityIn"
                } else {
                    "HighQualityIn"
                };
                tl_map.insert(f.into(), Value::from(u_quality_in));
            }
            if b_quality_out && u_quality_out != 0 {
                let f = if is_low {
                    "LowQualityOut"
                } else {
                    "HighQualityOut"
                };
                tl_map.insert(f.into(), Value::from(u_quality_out));
            }
            // Placeholder so the central PreviousTxnID stamping records this tx
            // on the newly created line (it only touches entries that already
            // expose the field).
            tl_map.insert(
                "PreviousTxnID".into(),
                "0000000000000000000000000000000000000000000000000000000000000000".into(),
            );
            tl_map.insert("PreviousTxnLgrSeq".into(), Value::from(0u32));
            let tl_obj = Value::Object(tl_map);
            let bytes = serde_json::to_vec(&tl_obj).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .insert(tl_key, bytes)
                .map_err(|_| TransactionResult::TemMalformed)?;

            // The creator always takes the reserve on create: increment its
            // OwnerCount. Its Sequence/Ticket is consumed centrally by the engine
            // before doApply.
            let acct_key = keylet::account(&account_id);
            if let Some(acct_bytes) = ctx.view.read(&acct_key) {
                let mut acct: Value = serde_json::from_slice(&acct_bytes)
                    .map_err(|_| TransactionResult::TemMalformed)?;
                helpers::adjust_owner_count(&mut acct, 1);
                let new_bytes =
                    serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
                ctx.view
                    .update(acct_key, new_bytes)
                    .map_err(|_| TransactionResult::TemMalformed)?;
            }

            // Adding the line to the issuer's owner directory re-threads the
            // issuer's account root in rippled (its PreviousTxnID becomes this
            // tx) even though no field on it changes. Re-write it unchanged so
            // central PreviousTxnID stamping touches it too.
            let issuer_key = keylet::account(&issuer_id);
            if let Some(issuer_bytes) = ctx.view.read(&issuer_key) {
                let _ = ctx.view.update(issuer_key, issuer_bytes);
            }
        }

        Ok(TransactionResult::TesSuccess)
    }
}

/// Convert a 3-letter currency code to 20 bytes (zero-padded, offset by 12).
fn currency_to_bytes(currency: &str) -> [u8; 20] {
    let mut bytes = [0u8; 20];
    let code = currency.as_bytes();
    if code.len() == 3 {
        bytes[12] = code[0];
        bytes[13] = code[1];
        bytes[14] = code[2];
    } else if code.len() == 40 {
        // Hex-encoded 20-byte currency
        if let Ok(decoded) = hex::decode(currency) {
            if decoded.len() == 20 {
                bytes.copy_from_slice(&decoded);
            }
        }
    }
    bytes
}
