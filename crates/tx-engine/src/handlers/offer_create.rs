use rxrpl_amendment::feature::feature_id;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::AccountId;
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

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

        // Create the offer ledger entry
        let offer_key = keylet::offer(&account_id, sequence);
        let offer_obj = serde_json::json!({
            "LedgerEntryType": "Offer",
            "Account": account_str,
            "Sequence": sequence,
            "TakerPays": ctx.tx["TakerPays"],
            "TakerGets": ctx.tx["TakerGets"],
            "Flags": ctx.tx.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0),
        });

        let offer_bytes =
            serde_json::to_vec(&offer_obj).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .insert(offer_key, offer_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        // Update account: increment sequence and owner count
        helpers::increment_sequence(&mut acct);
        helpers::adjust_owner_count(&mut acct, 1);

        let new_bytes = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .update(acct_key, new_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        Ok(TransactionResult::TesSuccess)
    }
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

    let issuer_id =
        decode_account_id(issuer_str).map_err(|_| TransactionResult::TemMalformed)?;
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

        if let Some(accepted) = domain
            .get("AcceptedCredentials")
            .and_then(|v| v.as_array())
        {
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
                        let cred_key =
                            keylet::credential(trader_id, &ci_id, ct.as_bytes());
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
