use rxrpl_amendment::feature::feature_id;
use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_crypto::sha512_half::sha512_half;
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct DepositPreauthTransactor;

type CredentialPair = (AccountId, Vec<u8>);

/// rippled's checkArray + makeSorted for credential-based preauthorization.
fn credential_pairs(
    tx: &Value,
    field: &str,
) -> Result<Option<Vec<CredentialPair>>, TransactionResult> {
    let Some(value) = tx.get(field) else {
        return Ok(None);
    };
    let entries = value.as_array().ok_or(TransactionResult::TemMalformed)?;
    if entries.is_empty() {
        return Err(TransactionResult::TemArrayEmpty);
    }
    if entries.len() > 8 {
        return Err(TransactionResult::TemArrayTooLarge);
    }
    let mut pairs = Vec::with_capacity(entries.len());
    for entry in entries {
        let credential = entry
            .get("Credential")
            .ok_or(TransactionResult::TemMalformed)?;
        let issuer = credential
            .get("Issuer")
            .and_then(Value::as_str)
            .ok_or(TransactionResult::TemInvalidAccountId)
            .and_then(|address| {
                decode_account_id(address).map_err(|_| TransactionResult::TemInvalidAccountId)
            })?;
        if issuer == AccountId::ZERO {
            return Err(TransactionResult::TemInvalidAccountId);
        }
        let credential_type = credential
            .get("CredentialType")
            .and_then(Value::as_str)
            .ok_or(TransactionResult::TemMalformed)
            .and_then(|value| hex::decode(value).map_err(|_| TransactionResult::TemMalformed))?;
        if credential_type.is_empty() || credential_type.len() > 64 {
            return Err(TransactionResult::TemMalformed);
        }
        pairs.push((issuer, credential_type));
    }
    pairs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()).then(a.1.cmp(&b.1)));
    if pairs.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(TransactionResult::TemMalformed);
    }
    Ok(Some(pairs))
}

/// keylet::depositPreauth(owner, credentials): `P || owner ||
/// sha512Half(issuer || type)* || credential_count(u64 BE)`.
fn credential_preauth_key(owner: &AccountId, pairs: &[CredentialPair]) -> Hash256 {
    let mut hashes = Vec::with_capacity(pairs.len() * 32);
    for (issuer, credential_type) in pairs {
        hashes.extend_from_slice(sha512_half(&[issuer.as_bytes(), credential_type]).as_bytes());
    }
    let count = (pairs.len() as u64).to_be_bytes();
    sha512_half(&[&[0, b'P'], owner.as_bytes(), &hashes, &count])
}

impl Transactor for DepositPreauthTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let has_authorize = helpers::get_str_field(ctx.tx, "Authorize").is_some();
        let has_unauthorize = helpers::get_str_field(ctx.tx, "Unauthorize").is_some();
        let has_authorize_credentials = ctx.tx.get("AuthorizeCredentials").is_some();
        let has_unauthorize_credentials = ctx.tx.get("UnauthorizeCredentials").is_some();

        if (has_authorize_credentials || has_unauthorize_credentials)
            && !ctx.rules.enabled(&feature_id("Credentials"))
        {
            return Err(TransactionResult::TemDisabled);
        }
        // Exactly one of the four supported authorization operations.
        if [
            has_authorize,
            has_unauthorize,
            has_authorize_credentials,
            has_unauthorize_credentials,
        ]
        .into_iter()
        .filter(|present| *present)
        .count()
            != 1
        {
            return Err(TransactionResult::TemMalformed);
        }

        credential_pairs(ctx.tx, "AuthorizeCredentials")?;
        credential_pairs(ctx.tx, "UnauthorizeCredentials")?;

        if has_authorize || has_unauthorize {
            let account = helpers::get_account(ctx.tx)?;
            let target = if has_authorize {
                helpers::get_str_field(ctx.tx, "Authorize").unwrap()
            } else {
                helpers::get_str_field(ctx.tx, "Unauthorize").unwrap()
            };
            if account == target {
                return Err(TransactionResult::TemCannotPreAuthSelf);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        if let Some(authorize) = helpers::get_str_field(ctx.tx, "Authorize") {
            // Target account must exist
            helpers::read_account_by_address(ctx.view, authorize)?;

            // Check for duplicate preauth
            let owner_id = decode_account_id(account_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let auth_id =
                decode_account_id(authorize).map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let dp_key = keylet::deposit_preauth(&owner_id, &auth_id);
            if ctx.view.exists(&dp_key) {
                return Err(TransactionResult::TecDuplicate);
            }
        }

        if let Some(unauthorize) = helpers::get_str_field(ctx.tx, "Unauthorize") {
            let owner_id = decode_account_id(account_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let auth_id = decode_account_id(unauthorize)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let dp_key = keylet::deposit_preauth(&owner_id, &auth_id);
            if !ctx.view.exists(&dp_key) {
                return Err(TransactionResult::TecNoEntry);
            }
        }

        let owner_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        if let Some(pairs) = credential_pairs(ctx.tx, "AuthorizeCredentials")? {
            for (issuer, _) in &pairs {
                if !ctx.view.exists(&keylet::account(issuer)) {
                    return Err(TransactionResult::TecNoIssuer);
                }
            }
            if ctx.view.exists(&credential_preauth_key(&owner_id, &pairs)) {
                return Err(TransactionResult::TecDuplicate);
            }
        }
        if let Some(pairs) = credential_pairs(ctx.tx, "UnauthorizeCredentials")?
            && !ctx.view.exists(&credential_preauth_key(&owner_id, &pairs))
        {
            return Err(TransactionResult::TecNoEntry);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Update sender account (sequence + owner count)
        let account_key = keylet::account(&account_id);
        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let authorize_credentials = credential_pairs(ctx.tx, "AuthorizeCredentials")?;
        let unauthorize_credentials = credential_pairs(ctx.tx, "UnauthorizeCredentials")?;
        if helpers::get_str_field(ctx.tx, "Authorize").is_some() || authorize_credentials.is_some()
        {
            // The reserve check uses the starting balance, before the fee.
            let pre_fee_balance =
                helpers::get_balance(&account).saturating_add(helpers::get_fee(ctx.tx));
            if pre_fee_balance
                < ctx
                    .fees
                    .account_reserve(helpers::get_owner_count(&account) + 1)
            {
                return Ok(TransactionResult::TecInsufficientReserve);
            }
        }

        if let Some(authorize) = helpers::get_str_field(ctx.tx, "Authorize") {
            let auth_id =
                decode_account_id(authorize).map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let dp_key = keylet::deposit_preauth(&account_id, &auth_id);

            // Link into the owner directory first so the resulting page index can
            // be written as the SoeRequired sfOwnerNode. rippled builds the SLE
            // from the ledger-entry template, so the default sfFlags=0 (a
            // SoeRequired common field) is serialized too.
            let owner_node = crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &dp_key)?;

            let entry = serde_json::json!({
                "LedgerEntryType": "DepositPreauth",
                "Account": account_str,
                "Authorize": authorize,
                // Default sfFlags is serialized (SoeRequired common field).
                "Flags": 0u32,
                // Owner-directory page hint (SoeRequired).
                "OwnerNode": format!("{owner_node:016X}"),
                // Placeholder filled by the engine's central PreviousTxnID stamping.
                "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
                "PreviousTxnLgrSeq": 0,
            });
            let entry_data =
                serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .insert(dp_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            helpers::adjust_owner_count(&mut account, 1);
        }

        if let Some(unauthorize) = helpers::get_str_field(ctx.tx, "Unauthorize") {
            let auth_id = decode_account_id(unauthorize)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let dp_key = keylet::deposit_preauth(&account_id, &auth_id);

            crate::owner_dir::remove_from_owner_dir(ctx.view, &account_id, &dp_key)?;
            ctx.view
                .erase(&dp_key)
                .map_err(|_| TransactionResult::TefInternal)?;

            helpers::adjust_owner_count(&mut account, -1);
        }

        if let Some(pairs) = authorize_credentials {
            let dp_key = credential_preauth_key(&account_id, &pairs);
            let owner_node = crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &dp_key)?;
            let sorted_credentials: Vec<Value> = pairs
                .iter()
                .map(|(issuer, credential_type)| {
                    serde_json::json!({"Credential": {
                        "Issuer": encode_account_id(issuer),
                        "CredentialType": hex::encode_upper(credential_type),
                    }})
                })
                .collect();
            let entry = serde_json::json!({
                "LedgerEntryType": "DepositPreauth",
                "Account": account_str,
                "AuthorizeCredentials": sorted_credentials,
                "Flags": 0u32,
                "OwnerNode": format!("{owner_node:016X}"),
                "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
                "PreviousTxnLgrSeq": 0,
            });
            let data = serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .insert(dp_key, data)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut account, 1);
        }
        if let Some(pairs) = unauthorize_credentials {
            let dp_key = credential_preauth_key(&account_id, &pairs);
            crate::owner_dir::remove_from_owner_dir(ctx.view, &account_id, &dp_key)?;
            ctx.view
                .erase(&dp_key)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut account, -1);
        }

        let account_data =
            serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(account_key, account_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
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

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const TARGET: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_two_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(OWNER, 100_000_000u64), (TARGET, 50_000_000)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        ledger
    }

    #[test]
    fn ledger_105502830_authorize_credentials_matches_oracle_key() {
        const CREDENTIAL_TYPE: &str = "787370656E63652E6D616B6577617665732E32303236";
        const ORACLE_KEY: &str = "BC10BBAE8ADCC6626E2B820FE37E2D6626CAC41A23B7639193CAD62B488E0BE0";
        const ACCOUNT: &str = "rLx9S9JPW9pmN9VeiveytN8AtHQMEGnp8D";
        let mut ledger = Ledger::genesis();
        let owner_id = decode_account_id(ACCOUNT).unwrap();
        ledger
            .put_state(
                keylet::account(&owner_id),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AccountRoot", "Account": ACCOUNT,
                    "Balance": "49999882", "Sequence": 105501017,
                    "OwnerCount": 3, "Flags": 16777216,
                }))
                .unwrap(),
            )
            .unwrap();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::from_enabled([rxrpl_amendment::feature::feature_id("Credentials")]);
        let credentials = serde_json::json!([{"Credential": {
            "CredentialType": CREDENTIAL_TYPE, "Issuer": ACCOUNT,
        }}]);
        let tx = serde_json::json!({
            "TransactionType": "DepositPreauth", "Account": ACCOUNT,
            "AuthorizeCredentials": credentials, "Fee": "12", "Sequence": 105501016,
            "Flags": 0,
        });
        let preflight = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(DepositPreauthTransactor.preflight(&preflight), Ok(()));
        let preclaim = PreclaimContext {
            tx: &tx,
            view: &sandbox,
            rules: &rules,
        };
        assert_eq!(DepositPreauthTransactor.preclaim(&preclaim), Ok(()));
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            DepositPreauthTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        let preauth_key: rxrpl_primitives::Hash256 = ORACLE_KEY.parse().unwrap();
        assert_eq!(
            credential_preauth_key(
                &owner_id,
                &credential_pairs(&tx, "AuthorizeCredentials")
                    .unwrap()
                    .unwrap()
            ),
            preauth_key
        );
        let bytes = sandbox.read(&preauth_key).expect("oracle preauth key");
        let entry: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(entry["AuthorizeCredentials"], credentials);
        assert_eq!(entry["OwnerNode"], "0000000000000000");
        let account: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&owner_id)).unwrap()).unwrap();
        assert_eq!(account["OwnerCount"], 4);

        let duplicate = PreclaimContext {
            tx: &tx,
            view: &sandbox,
            rules: &rules,
        };
        assert_eq!(
            DepositPreauthTransactor.preclaim(&duplicate),
            Err(TransactionResult::TecDuplicate)
        );
        let remove = serde_json::json!({
            "TransactionType": "DepositPreauth", "Account": ACCOUNT,
            "UnauthorizeCredentials": credentials, "Fee": "12", "Sequence": 105501017,
            "Flags": 0,
        });
        let remove_preclaim = PreclaimContext {
            tx: &remove,
            view: &sandbox,
            rules: &rules,
        };
        assert_eq!(DepositPreauthTransactor.preclaim(&remove_preclaim), Ok(()));
        let mut remove_ctx = ApplyContext {
            tx: &remove,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            DepositPreauthTransactor.apply(&mut remove_ctx),
            Ok(TransactionResult::TesSuccess)
        );
        assert!(!sandbox.exists(&preauth_key));
        let account: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&owner_id)).unwrap()).unwrap();
        assert_eq!(account["OwnerCount"], 3);
    }

    #[test]
    fn authorize_reserve_uses_pre_fee_balance_for_both_variants() {
        let fees = FeeSettings::default();
        let reserve = fees.account_reserve(1);
        let rules = Rules::from_enabled([feature_id("Credentials")]);
        for credentials in [false, true] {
            for (starting_balance, expected) in [
                (reserve - 1, TransactionResult::TecInsufficientReserve),
                (reserve, TransactionResult::TesSuccess),
            ] {
                let owner_id = decode_account_id(OWNER).unwrap();
                let mut ledger = Ledger::genesis();
                ledger
                    .put_state(
                        keylet::account(&owner_id),
                        serde_json::to_vec(&serde_json::json!({
                            "LedgerEntryType": "AccountRoot", "Account": OWNER,
                            "Balance": (starting_balance - 12).to_string(),
                            "Sequence": 2, "OwnerCount": 0, "Flags": 0,
                        }))
                        .unwrap(),
                    )
                    .unwrap();
                let view = LedgerView::with_fees(&ledger, fees.clone());
                let mut sandbox = Sandbox::new(&view);
                let tx = if credentials {
                    serde_json::json!({
                        "TransactionType": "DepositPreauth", "Account": OWNER,
                        "AuthorizeCredentials": [{"Credential": {
                            "Issuer": OWNER, "CredentialType": "01"
                        }}],
                        "Fee": "12", "Sequence": 1,
                    })
                } else {
                    serde_json::json!({
                        "TransactionType": "DepositPreauth", "Account": OWNER,
                        "Authorize": TARGET, "Fee": "12", "Sequence": 1,
                    })
                };
                let mut ctx = ApplyContext {
                    tx: &tx,
                    view: &mut sandbox,
                    rules: &rules,
                    fees: &fees,
                };
                assert_eq!(
                    DepositPreauthTransactor.apply(&mut ctx),
                    Ok(expected),
                    "credentials={credentials}, starting_balance={starting_balance}"
                );
                let account: Value =
                    serde_json::from_slice(&sandbox.read(&keylet::account(&owner_id)).unwrap())
                        .unwrap();
                assert_eq!(
                    account["OwnerCount"],
                    if expected == TransactionResult::TesSuccess {
                        1
                    } else {
                        0
                    }
                );
            }
        }
    }

    #[test]
    fn preflight_both_authorize_and_unauthorize() {
        let tx = serde_json::json!({
            "TransactionType": "DepositPreauth",
            "Account": OWNER,
            "Authorize": TARGET,
            "Unauthorize": TARGET,
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
            DepositPreauthTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_self_preauth() {
        let tx = serde_json::json!({
            "TransactionType": "DepositPreauth",
            "Account": OWNER,
            "Authorize": OWNER,
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
            DepositPreauthTransactor.preflight(&ctx),
            Err(TransactionResult::TemCannotPreAuthSelf)
        );
    }

    #[test]
    fn apply_authorize_then_unauthorize() {
        let ledger = setup_two_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        // Authorize
        let tx = serde_json::json!({
            "TransactionType": "DepositPreauth",
            "Account": OWNER,
            "Authorize": TARGET,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = DepositPreauthTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify entry exists
        let owner_id = decode_account_id(OWNER).unwrap();
        let target_id = decode_account_id(TARGET).unwrap();
        let dp_key = keylet::deposit_preauth(&owner_id, &target_id);
        assert!(sandbox.exists(&dp_key));

        // The created DepositPreauth carries the default sfFlags=0 and the
        // owner-directory page hint (sfOwnerNode), both serialized to match
        // rippled's account_hash.
        let dp_bytes = sandbox.read(&dp_key).unwrap();
        let dp: serde_json::Value = serde_json::from_slice(&dp_bytes).unwrap();
        assert_eq!(dp["Flags"].as_u64().unwrap(), 0);
        assert_eq!(dp["OwnerNode"].as_str().unwrap(), "0000000000000000");

        // Verify owner count
        let owner_key = keylet::account(&owner_id);
        let owner_bytes = sandbox.read(&owner_key).unwrap();
        let owner: serde_json::Value = serde_json::from_slice(&owner_bytes).unwrap();
        assert_eq!(owner["OwnerCount"].as_u64().unwrap(), 1);

        // Unauthorize
        let tx2 = serde_json::json!({
            "TransactionType": "DepositPreauth",
            "Account": OWNER,
            "Unauthorize": TARGET,
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx2 = ApplyContext {
            tx: &tx2,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result2 = DepositPreauthTransactor.apply(&mut ctx2).unwrap();
        assert_eq!(result2, TransactionResult::TesSuccess);

        // Entry deleted
        assert!(!sandbox.exists(&dp_key));

        // Owner count back to 0
        let owner_bytes = sandbox.read(&owner_key).unwrap();
        let owner: serde_json::Value = serde_json::from_slice(&owner_bytes).unwrap();
        assert_eq!(owner["OwnerCount"].as_u64().unwrap(), 0);
    }
}
