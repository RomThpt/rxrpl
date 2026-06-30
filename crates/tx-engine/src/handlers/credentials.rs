//! Shared helpers for the Credential transactor family (CredentialCreate /
//! CredentialAccept / CredentialDelete).
//!
//! Mirrors rippled's `libxrpl/ledger/helpers/CredentialHelpers.cpp` and the
//! ledger-entry field set in `Credential.h`. The `Credential` SLE always
//! serializes `sfFlags` and `sfIssuerNode` (SoeRequired) and serializes
//! `sfSubjectNode` only when the subject differs from the issuer (SoeOptional,
//! set just for the non-self-issued case).

use rxrpl_amendment::Rules;
use rxrpl_amendment::feature::feature_id;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::owner_dir::remove_from_owner_dir_page;
use crate::view::apply_view::ApplyView;

/// `lsfAccepted` (0x00010000): set on a credential once its subject accepts it,
/// or immediately for a self-issued credential (subject == issuer).
pub const LSF_ACCEPTED: u32 = 0x0001_0000;

/// Maximum `sfCredentialType` length in bytes (rippled `kMaxCredentialTypeLength`).
pub const MAX_CREDENTIAL_TYPE_LEN: usize = 64;

/// Maximum `sfURI` length in bytes (rippled `kMaxCredentialUriLength`).
pub const MAX_CREDENTIAL_URI_LEN: usize = 256;

/// `tfFullyCanonicalSig | tfInnerBatchTxn` — the only flags the Credential
/// transactors accept once `fixInvalidTxFlags` is active (rippled `tfUniversal`).
const TF_UNIVERSAL: u32 = 0xC000_0000;

/// rippled framework gates: the `Credentials` amendment must be enabled
/// (temDISABLED otherwise) and — once `fixInvalidTxFlags` is active — no flag
/// outside `tfUniversal` may be set (temINVALID_FLAG). Both run before the
/// transactor's own field checks, matching rippled's
/// `invoke_preflight` + `preflight0`.
pub fn preflight_gate(rules: &Rules, tx: &Value) -> Result<(), TransactionResult> {
    if !rules.enabled(&feature_id("Credentials")) {
        return Err(TransactionResult::TemDisabled);
    }
    if rules.enabled(&feature_id("fixInvalidTxFlags")) {
        let flags = helpers::get_u32_field(tx, "Flags").unwrap_or(0);
        if flags & !TF_UNIVERSAL != 0 {
            return Err(TransactionResult::TemInvalidFlag);
        }
    }
    Ok(())
}

/// Validate `sfCredentialType`: present, non-empty, ≤ 64 bytes. Returns the
/// decoded bytes (the keylet hashes the decoded blob, not the hex string).
pub fn validate_credential_type(tx: &Value) -> Result<Vec<u8>, TransactionResult> {
    let ct = helpers::get_str_field(tx, "CredentialType").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(ct).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_TYPE_LEN {
        return Err(TransactionResult::TemMalformed);
    }
    Ok(bytes)
}

/// Decode an account field that must not be the zeroed AccountID
/// (temINVALID_ACCOUNT_ID, mirroring rippled's `isZero()` check).
pub fn decode_non_zero_account(addr: &str) -> Result<AccountId, TransactionResult> {
    let id = decode_account_id(addr).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    if id == AccountId::ZERO {
        return Err(TransactionResult::TemInvalidAccountId);
    }
    Ok(id)
}

/// rippled `credentials::checkExpired`: a credential is expired when the
/// ledger's parent close time is strictly past its `Expiration`. A credential
/// without `Expiration` never expires.
pub fn check_expired(cred: &Value, parent_close_time: u32) -> bool {
    let exp = cred
        .get("Expiration")
        .and_then(|v| v.as_u64())
        .unwrap_or(u64::from(u32::MAX)) as u32;
    parent_close_time > exp
}

fn node_page(cred: &Value, field: &str) -> u64 {
    cred.get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| u64::from_str_radix(s, 16).ok())
        .unwrap_or(0)
}

/// rippled `credentials::deleteSLE`: unlink a credential from the issuer (and,
/// when the subject is distinct, the subject) owner directory using the page
/// hints recorded on the SLE (`sfIssuerNode` / `sfSubjectNode`), adjust the
/// reserve holder's owner count, then erase the SLE.
///
/// Owner-count attribution mirrors rippled exactly: the issuer is charged while
/// the credential is unaccepted (or self-issued); the subject is charged once it
/// has been accepted.
pub fn delete_credential(
    view: &mut dyn ApplyView,
    cred_key: &Hash256,
    cred: &Value,
) -> Result<(), TransactionResult> {
    let issuer_str = cred
        .get("Issuer")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TefInternal)?;
    let subject_str = cred
        .get("Subject")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TefInternal)?;
    let issuer_id = decode_account_id(issuer_str).map_err(|_| TransactionResult::TefInternal)?;
    let subject_id = decode_account_id(subject_str).map_err(|_| TransactionResult::TefInternal)?;
    let accepted = helpers::get_flags(cred) & LSF_ACCEPTED != 0;

    // Issuer side: charged while unaccepted, or when self-issued.
    delete_side(
        view,
        &issuer_id,
        node_page(cred, "IssuerNode"),
        cred_key,
        !accepted || subject_id == issuer_id,
    )?;

    // Subject side (only linked when distinct): charged once accepted.
    if subject_id != issuer_id {
        delete_side(
            view,
            &subject_id,
            node_page(cred, "SubjectNode"),
            cred_key,
            accepted,
        )?;
    }

    view.erase(cred_key)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
}

fn delete_side(
    view: &mut dyn ApplyView,
    account_id: &AccountId,
    page: u64,
    cred_key: &Hash256,
    is_owner: bool,
) -> Result<(), TransactionResult> {
    remove_from_owner_dir_page(view, account_id, page, cred_key)?;
    if is_owner {
        let acct_key = keylet::account(account_id);
        let bytes = view.read(&acct_key).ok_or(TransactionResult::TefInternal)?;
        let mut acct: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut acct, -1);
        let data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
        view.update(acct_key, data)
            .map_err(|_| TransactionResult::TefInternal)?;
    }
    Ok(())
}
