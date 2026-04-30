use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::helpers;
use crate::owner_dir::add_to_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

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
        let limit_zero =
            limit_value == "0" || limit_value.parse::<f64>().map(|f| f == 0.0).unwrap_or(false);
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
        let issuer_account: serde_json::Value = serde_json::from_slice(&issuer_bytes)
            .map_err(|_| TransactionResult::TefInternal)?;
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

        // Check if trust line exists
        let existing = ctx.view.read(&tl_key);

        if let Some(bytes) = existing {
            // Update existing trust line
            let mut obj: Value =
                serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;

            // Determine which side we are (low or high)
            let is_low = account_id.as_bytes() < issuer_id.as_bytes();
            if is_low {
                obj["LowLimit"] = limit.clone();
            } else {
                obj["HighLimit"] = limit.clone();
            }

            // Apply quality settings if present
            if is_low {
                if let Some(qi) = ctx.tx.get("QualityIn") {
                    obj["LowQualityIn"] = qi.clone();
                }
                if let Some(qo) = ctx.tx.get("QualityOut") {
                    obj["LowQualityOut"] = qo.clone();
                }
            } else {
                if let Some(qi) = ctx.tx.get("QualityIn") {
                    obj["HighQualityIn"] = qi.clone();
                }
                if let Some(qo) = ctx.tx.get("QualityOut") {
                    obj["HighQualityOut"] = qo.clone();
                }
            }

            let new_bytes =
                serde_json::to_vec(&obj).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .update(tl_key, new_bytes)
                .map_err(|_| TransactionResult::TemMalformed)?;
        } else {
            // Create new trust line
            let is_low = account_id.as_bytes() < issuer_id.as_bytes();
            let zero_amount = serde_json::json!({
                "currency": limit["currency"],
                "issuer": issuer_str,
                "value": "0"
            });

            let (low_limit, high_limit) = if is_low {
                (limit.clone(), zero_amount)
            } else {
                (zero_amount, limit.clone())
            };

            let tl_obj = serde_json::json!({
                "LedgerEntryType": "RippleState",
                "Balance": {
                    "currency": limit["currency"],
                    "issuer": issuer_str,
                    "value": "0"
                },
                "LowLimit": low_limit,
                "HighLimit": high_limit,
                "Flags": 0,
            });

            let bytes = serde_json::to_vec(&tl_obj).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .insert(tl_key, bytes)
                .map_err(|_| TransactionResult::TemMalformed)?;

            // Link the new RippleState into the calling account's owner
            // directory so account_lines / account_objects can find it.
            add_to_owner_dir(ctx.view, &account_id, &tl_key)?;

            // Increment owner count for the account
            let acct_key = keylet::account(&account_id);
            if let Some(acct_bytes) = ctx.view.read(&acct_key) {
                let mut acct: Value = serde_json::from_slice(&acct_bytes)
                    .map_err(|_| TransactionResult::TemMalformed)?;
                helpers::adjust_owner_count(&mut acct, 1);
                helpers::increment_sequence(&mut acct);
                let new_bytes =
                    serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
                ctx.view
                    .update(acct_key, new_bytes)
                    .map_err(|_| TransactionResult::TemMalformed)?;
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
