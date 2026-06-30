use std::collections::HashSet;

use rxrpl_amendment::feature::feature_id;
use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct PermissionedDomainSetTransactor;

const ZERO_TXID: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// kMaxPermissionedDomainCredentialsArraySize (rippled Protocol.h).
const MAX_CREDENTIALS: usize = 10;
/// kMaxCredentialTypeLength (rippled Protocol.h).
const MAX_CREDENTIAL_TYPE_LEN: usize = 64;

/// Parse a 32-byte hex string (sfDomainID is a uint256, the domain object's
/// ledger key) into a [`Hash256`].
fn parse_hash256(s: &str) -> Result<Hash256, TransactionResult> {
    let bytes = hex::decode(s).map_err(|_| TransactionResult::TemMalformed)?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| TransactionResult::TemMalformed)?;
    Ok(Hash256::from(arr))
}

/// Decode an Issuer field (classic address or 40-char hex) into an `AccountId`.
fn decode_issuer(s: &str) -> Result<AccountId, TransactionResult> {
    if s.len() == 40 {
        let bytes = hex::decode(s).map_err(|_| TransactionResult::TemMalformed)?;
        AccountId::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
    } else {
        decode_account_id(s).map_err(|_| TransactionResult::TemInvalidAccountId)
    }
}

/// Extract and validate one `AcceptedCredentials` entry, returning the issuer
/// account and the raw credential-type bytes. Mirrors the per-credential checks
/// of rippled `credentials::checkArray`:
///   - zero issuer account -> temINVALID_ACCOUNT_ID
///   - empty / oversize CredentialType -> temMALFORMED
fn parse_credential(item: &Value) -> Result<(AccountId, Vec<u8>), TransactionResult> {
    let inner = item
        .get("Credential")
        .ok_or(TransactionResult::TemMalformed)?;
    let issuer_str = inner
        .get("Issuer")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TemMalformed)?;
    let issuer = decode_issuer(issuer_str)?;
    if issuer == AccountId::ZERO {
        return Err(TransactionResult::TemInvalidAccountId);
    }
    let ct_hex = inner
        .get("CredentialType")
        .and_then(|v| v.as_str())
        .ok_or(TransactionResult::TemMalformed)?;
    let ct = hex::decode(ct_hex).map_err(|_| TransactionResult::TemMalformed)?;
    if ct.is_empty() || ct.len() > MAX_CREDENTIAL_TYPE_LEN {
        return Err(TransactionResult::TemMalformed);
    }
    Ok((issuer, ct))
}

/// Sort + dedup the accepted credentials by (Issuer, CredentialType), emitting
/// minimal inner `{Issuer, CredentialType}` objects. Mirrors rippled
/// `credentials::makeSorted` (a `std::set<std::pair<AccountID, Slice>>`): the
/// issuer is compared by its 20 raw bytes and the credential type lexically.
fn make_sorted(creds: &[(AccountId, Vec<u8>)]) -> Vec<Value> {
    let mut sorted: Vec<&(AccountId, Vec<u8>)> = creds.iter().collect();
    sorted.sort_by(|a, b| a.0.0.cmp(&b.0.0).then_with(|| a.1.cmp(&b.1)));
    sorted.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    sorted
        .into_iter()
        .map(|(issuer, ct)| {
            serde_json::json!({
                "Credential": {
                    "Issuer": encode_account_id(issuer),
                    "CredentialType": hex::encode_upper(ct),
                }
            })
        })
        .collect()
}

impl Transactor for PermissionedDomainSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Amendment gates: the transaction's own feature
        // (featurePermissionedDomains) and the accepted-credentials feature
        // (featureCredentials, via checkExtraFeatures) must both be enabled.
        if !ctx.rules.enabled(&feature_id("PermissionedDomains")) {
            return Err(TransactionResult::TemDisabled);
        }
        if !ctx.rules.enabled(&feature_id("Credentials")) {
            return Err(TransactionResult::TemDisabled);
        }

        // credentials::checkArray
        let credentials = helpers::get_array_field(ctx.tx, "AcceptedCredentials")
            .ok_or(TransactionResult::TemMalformed)?;
        if credentials.is_empty() {
            return Err(TransactionResult::TemArrayEmpty);
        }
        if credentials.len() > MAX_CREDENTIALS {
            return Err(TransactionResult::TemArrayTooLarge);
        }
        let mut seen: HashSet<([u8; 20], Vec<u8>)> = HashSet::new();
        for item in credentials {
            let (issuer, ct) = parse_credential(item)?;
            if !seen.insert((issuer.0, ct)) {
                // Duplicate (Issuer, CredentialType).
                return Err(TransactionResult::TemMalformed);
            }
        }

        // A modify must reference a non-zero DomainID.
        if let Some(domain) = helpers::get_str_field(ctx.tx, "DomainID") {
            if parse_hash256(domain)? == Hash256::ZERO {
                return Err(TransactionResult::TemMalformed);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        // The sender must exist. All other checks (issuer existence, domain
        // existence/ownership, reserve) are deferred to apply so their `tec`
        // results are CLAIMED (fee + sequence charged), matching rippled.
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Re-parse the credential array (already validated in preflight).
        let raw = helpers::get_array_field(ctx.tx, "AcceptedCredentials")
            .ok_or(TransactionResult::TemMalformed)?;
        let mut creds: Vec<(AccountId, Vec<u8>)> = Vec::with_capacity(raw.len());
        for item in raw {
            creds.push(parse_credential(item)?);
        }

        // tecNO_ISSUER: every referenced issuer account must exist (rippled
        // preclaim, applied to both create and modify, and claimed here).
        for (issuer, _) in &creds {
            if !ctx.view.exists(&keylet::account(issuer)) {
                return Err(TransactionResult::TecNoIssuer);
            }
        }

        let sorted_credentials = Value::Array(make_sorted(&creds));

        if let Some(domain_hex) = helpers::get_str_field(ctx.tx, "DomainID") {
            // ---- Modify an existing permissioned domain. ----
            // sfDomainID IS the domain object's ledger key (uint256).
            let domain_key = parse_hash256(domain_hex)?;
            let entry_bytes = ctx
                .view
                .read(&domain_key)
                .ok_or(TransactionResult::TecNoEntry)?;
            let mut entry: Value =
                serde_json::from_slice(&entry_bytes).map_err(|_| TransactionResult::TefInternal)?;

            // Only the owner may modify the domain.
            let owner_ok = entry
                .get("Owner")
                .and_then(|v| v.as_str())
                .and_then(|s| decode_account_id(s).ok())
                .map(|o| o == account_id)
                .unwrap_or(false);
            if !owner_ok {
                return Err(TransactionResult::TecNoPermission);
            }

            entry["AcceptedCredentials"] = sorted_credentials;
            let entry_data =
                serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(domain_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            // ---- Create a new permissioned domain. ----
            let account_key = keylet::account(&account_id);
            let account_bytes = ctx
                .view
                .read(&account_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut account: Value = serde_json::from_slice(&account_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;

            // Reserve check for the new owned object (post-fee balance).
            let owner_count = helpers::get_owner_count(&account);
            let reserve = ctx.fees.account_reserve(owner_count + 1);
            if helpers::get_balance(&account) < reserve {
                return Err(TransactionResult::TecInsufficientReserve);
            }

            // The new domain's keylet derives from account + the tx's
            // sequence/ticket (rippled getSeqValue), and the SLE records that
            // same sequence.
            let seq = helpers::tx_seq_proxy_value(ctx.tx);
            let domain_key = keylet::permissioned_domain(&account_id, seq);

            let mut entry = serde_json::json!({
                "LedgerEntryType": "PermissionedDomain",
                "Flags": 0,
                "Owner": account_str,
                "Sequence": seq,
                "AcceptedCredentials": sorted_credentials,
                "PreviousTxnID": ZERO_TXID,
                "PreviousTxnLgrSeq": 0,
            });

            // Link into the owner directory; the page becomes sfOwnerNode.
            // (Captured BEFORE insert.) rippled serializes both sfFlags and
            // sfOwnerNode on the PermissionedDomain SLE even when zero (verified
            // byte-for-byte against the validated account_hash), so write
            // OwnerNode unconditionally — including "0000000000000000" on page 0.
            let owner_node =
                crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &domain_key)?;
            entry["OwnerNode"] = Value::String(format!("{owner_node:016X}"));

            let entry_data =
                serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .insert(domain_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            helpers::adjust_owner_count(&mut account, 1);
            let account_data =
                serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(account_key, account_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        Ok(TransactionResult::TesSuccess)
    }
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

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    // Two distinct, real issuer accounts. Their AccountIDs order ISS_B
    // (2C95..) < ISS_A (778E..), so supplying them A-then-B exercises the
    // makeSorted reordering.
    const ISS_A: &str = "rBu91aANPBsfQ9GR8dJ28CwKtnEVR4MMhN";
    const ISS_B: &str = "rnhjKVLR4iVoYba5Qmu1mYPJoupTKEVMRb";

    fn rules() -> Rules {
        Rules::from_enabled([feature_id("PermissionedDomains"), feature_id("Credentials")])
    }

    fn cred(issuer: &str, ct_hex: &str) -> serde_json::Value {
        serde_json::json!({"Credential": {"Issuer": issuer, "CredentialType": ct_hex}})
    }

    fn put_account(ledger: &mut Ledger, addr: &str, owner_count: u64) {
        let id = decode_account_id(addr).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": addr,
            "Balance": "100000000",
            "Sequence": 5,
            "OwnerCount": owner_count,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
    }

    fn setup_account() -> Ledger {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ALICE, 0);
        // Issuer accounts must exist for the tecNO_ISSUER gate to pass.
        put_account(&mut ledger, ISS_A, 0);
        put_account(&mut ledger, ISS_B, 0);
        ledger
    }

    #[test]
    fn preflight_missing_credentials_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = rules();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_empty_credentials_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": [],
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = rules();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemArrayEmpty)
        );
    }

    #[test]
    fn preflight_too_many_credentials_rejects() {
        let creds: Vec<serde_json::Value> = (0..11)
            .map(|i| cred(ISS_A, &format!("{:02X}", i + 1)))
            .collect();
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": creds,
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = rules();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemArrayTooLarge)
        );
    }

    #[test]
    fn preflight_amendment_disabled_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": [cred(ISS_A, "4B5943")],
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = Rules::new();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemDisabled)
        );
    }

    #[test]
    fn preflight_empty_credential_type_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": [cred(ISS_A, "")],
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = rules();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_duplicate_credential_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": [cred(ISS_A, "4B5943"), cred(ISS_A, "4B5943")],
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = rules();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_zero_domain_id_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "DomainID": "0000000000000000000000000000000000000000000000000000000000000000",
            "AcceptedCredentials": [cred(ISS_A, "4B5943")],
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = rules();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn apply_creates_domain_with_sorted_credentials_and_owner_node() {
        let ledger = setup_account();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        // Supply credentials OUT of canonical order: ISS_A precedes ISS_B in the
        // tx, but ISS_B's AccountID sorts first, so makeSorted must reorder them.
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": [cred(ISS_A, "4B594341"), cred(ISS_B, "4B594342")],
            "Fee": "12",
            "Sequence": 5,
        });

        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PermissionedDomainSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let id = decode_account_id(ALICE).unwrap();
        let domain_key = keylet::permissioned_domain(&id, 5);
        assert!(sandbox.exists(&domain_key));

        let entry: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&domain_key).unwrap()).unwrap();
        assert_eq!(entry["Owner"].as_str().unwrap(), ALICE);
        assert_eq!(entry["Sequence"].as_u64().unwrap(), 5);
        // rippled serializes sfFlags and sfOwnerNode even when zero; the first
        // object lands on page 0, so OwnerNode is "0000000000000000".
        assert_eq!(entry["Flags"].as_u64().unwrap(), 0);
        assert_eq!(entry["OwnerNode"].as_str().unwrap(), "0000000000000000");

        // makeSorted reordered: ISS_B (sorts first) then ISS_A.
        let creds = entry["AcceptedCredentials"].as_array().unwrap();
        assert_eq!(creds.len(), 2);
        let iss_b_id = decode_account_id(ISS_B).unwrap();
        let iss_a_id = decode_account_id(ISS_A).unwrap();
        assert_eq!(
            creds[0]["Credential"]["Issuer"].as_str().unwrap(),
            encode_account_id(&iss_b_id)
        );
        assert_eq!(
            creds[1]["Credential"]["Issuer"].as_str().unwrap(),
            encode_account_id(&iss_a_id)
        );
        // CredentialType stored as uppercase hex.
        assert_eq!(
            creds[0]["Credential"]["CredentialType"].as_str().unwrap(),
            "4B594342"
        );

        let account_key = keylet::account(&id);
        let account: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&account_key).unwrap()).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(account["Sequence"].as_u64().unwrap(), 6);
    }

    #[test]
    fn apply_creates_domain_owner_node_when_dir_paged() {
        // Seed ALICE's owner directory with a full root page (32 entries) so the
        // new domain spills to page 1 and sfOwnerNode IS serialized (non-zero).
        let mut ledger = setup_account();
        let id = decode_account_id(ALICE).unwrap();
        let indexes: Vec<String> = (0..32u32)
            .map(|i| format!("{:064X}", 0xAAAA_0000u32 + i))
            .collect();
        let root = serde_json::json!({
            "LedgerEntryType": "DirectoryNode",
            "Owner": ALICE,
            "Flags": 0,
            "RootIndex": hex::encode_upper(keylet::owner_dir(&id).as_bytes()),
            "Indexes": indexes,
        });
        ledger
            .put_state(keylet::owner_dir(&id), serde_json::to_vec(&root).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": [cred(ISS_A, "4B594341")],
            "Fee": "12",
            "Sequence": 5,
        });
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PermissionedDomainSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let domain_key = keylet::permissioned_domain(&id, 5);
        let entry: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&domain_key).unwrap()).unwrap();
        // The new domain spilled to a non-zero page; OwnerNode must be present.
        assert_eq!(entry["OwnerNode"].as_str().unwrap(), "0000000000000001");
    }

    #[test]
    fn apply_modify_via_uint256_domain_id() {
        let mut ledger = setup_account();
        let id = decode_account_id(ALICE).unwrap();
        // The existing domain is keyed by its keylet (account + seq 3); the tx
        // references it by that 32-byte key.
        let domain_key = keylet::permissioned_domain(&id, 3);
        let existing = serde_json::json!({
            "LedgerEntryType": "PermissionedDomain",
            "Owner": ALICE,
            "Sequence": 3,
            "AcceptedCredentials": [cred(ISS_A, "4F4C44")],
            "PreviousTxnID": ZERO_TXID,
            "PreviousTxnLgrSeq": 0,
        });
        ledger
            .put_state(domain_key, serde_json::to_vec(&existing).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let domain_id_hex = hex::encode_upper(domain_key.as_bytes());
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "DomainID": domain_id_hex,
            "AcceptedCredentials": [cred(ISS_B, "4E4557")],
            "Fee": "12",
            "Sequence": 5,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PermissionedDomainSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let entry: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&domain_key).unwrap()).unwrap();
        let iss_b_id = decode_account_id(ISS_B).unwrap();
        assert_eq!(
            entry["AcceptedCredentials"][0]["Credential"]["Issuer"]
                .as_str()
                .unwrap(),
            encode_account_id(&iss_b_id)
        );

        // Owner count unchanged on modify.
        let account_key = keylet::account(&id);
        let account: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&account_key).unwrap()).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn apply_modify_wrong_owner_is_no_permission() {
        let mut ledger = setup_account();
        let id = decode_account_id(ALICE).unwrap();
        let domain_key = keylet::permissioned_domain(&id, 3);
        // Domain owned by a DIFFERENT account.
        let existing = serde_json::json!({
            "LedgerEntryType": "PermissionedDomain",
            "Owner": ISS_A,
            "Sequence": 3,
            "AcceptedCredentials": [cred(ISS_A, "4F4C44")],
            "PreviousTxnID": ZERO_TXID,
            "PreviousTxnLgrSeq": 0,
        });
        ledger
            .put_state(domain_key, serde_json::to_vec(&existing).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "DomainID": hex::encode_upper(domain_key.as_bytes()),
            "AcceptedCredentials": [cred(ISS_B, "4E4557")],
            "Fee": "12",
            "Sequence": 5,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.apply(&mut ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn apply_modify_absent_domain_is_no_entry() {
        let ledger = setup_account();
        let id = decode_account_id(ALICE).unwrap();
        let domain_key = keylet::permissioned_domain(&id, 99);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "DomainID": hex::encode_upper(domain_key.as_bytes()),
            "AcceptedCredentials": [cred(ISS_B, "4E4557")],
            "Fee": "12",
            "Sequence": 5,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.apply(&mut ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn apply_create_unknown_issuer_is_no_issuer() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ALICE, 0);
        // Note: issuer accounts deliberately NOT seeded.

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": [cred(ISS_A, "4B594341")],
            "Fee": "12",
            "Sequence": 5,
        });
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.apply(&mut ctx),
            Err(TransactionResult::TecNoIssuer)
        );
    }

    #[test]
    fn apply_create_below_reserve_is_insufficient_reserve() {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        // Balance below the one-object reserve.
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "1000000",
            "Sequence": 5,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        put_account(&mut ledger, ISS_A, 0);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": [cred(ISS_A, "4B594341")],
            "Fee": "12",
            "Sequence": 5,
        });
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainSetTransactor.apply(&mut ctx),
            Err(TransactionResult::TecInsufficientReserve)
        );
    }
}
