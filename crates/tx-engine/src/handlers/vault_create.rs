use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

const ZERO_TXID: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const LSF_DISABLE_MASTER: u32 = 0x0010_0000;
const LSF_DEFAULT_RIPPLE: u32 = 0x0080_0000;
const LSF_DEPOSIT_AUTH: u32 = 0x0100_0000;

const LSF_MPT_REQUIRE_AUTH: u32 = 0x0000_0004;
const LSF_MPT_CAN_ESCROW: u32 = 0x0000_0008;
const LSF_MPT_CAN_TRADE: u32 = 0x0000_0010;
const LSF_MPT_CAN_TRANSFER: u32 = 0x0000_0020;

const TF_VAULT_PRIVATE: u32 = 0x0001_0000;
const TF_VAULT_SHARE_NON_TRANSFERABLE: u32 = 0x0002_0000;

const VAULT_STRATEGY_FIRST_COME_FIRST_SERVE: u32 = 1;

pub struct VaultCreateTransactor;

/// The 192-bit share MPTokenIssuanceID is `sequence (4 bytes BE) || issuer (20
/// bytes)`, with sequence 1 (the pseudo-account's first and only issuance).
fn share_mptid(pseudo: &AccountId) -> String {
    let mut id = Vec::with_capacity(24);
    id.extend_from_slice(&1u32.to_be_bytes());
    id.extend_from_slice(pseudo.as_bytes());
    hex::encode_upper(id)
}

/// Derive the vault pseudo-account: the first `i` in `0..256` whose account
/// keylet is free, hashing `sha512Half(u16be(i) || parentHash || vaultKey)`.
fn derive_pseudo_account(
    ctx: &ApplyContext<'_>,
    vault_key: &Hash256,
) -> Result<AccountId, TransactionResult> {
    let parent = ctx.view.parent_hash();
    for i in 0u16..256 {
        let ibe = i.to_be_bytes();
        let hash = rxrpl_crypto::sha512_half::sha512_half(&[
            &ibe,
            parent.as_bytes(),
            vault_key.as_bytes(),
        ]);
        let id = rxrpl_codec::address::classic::account_id_from_public_key(hash.as_bytes());
        if !ctx.view.exists(&keylet::account(&id)) {
            return Ok(id);
        }
    }
    Err(TransactionResult::TecDuplicate)
}

/// True for an `Asset` that is XRP (the string `"XRP"` or `{"currency":"XRP"}`
/// with no issuer).
fn is_xrp_asset(asset: &serde_json::Value) -> bool {
    matches!(asset.as_str(), Some("XRP"))
        || (asset
            .get("currency")
            .and_then(|c| c.as_str())
            .map(|c| c == "XRP")
            .unwrap_or(false)
            && asset.get("issuer").is_none())
}

impl Transactor for VaultCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Asset is required
        let asset = ctx.tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;

        // Asset must be "XRP", {"currency":"XRP"}, or an IOU object with
        // currency+issuer.
        if !is_xrp_asset(asset) {
            if !asset.is_object() {
                return Err(TransactionResult::TemMalformed);
            }
            asset
                .get("currency")
                .and_then(|v| v.as_str())
                .filter(|c| !c.is_empty())
                .ok_or(TransactionResult::TemBadCurrency)?;
            asset
                .get("issuer")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TemBadIssuer)?;
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let seq = helpers::get_sequence(&account);
        let asset = ctx.tx.get("Asset").unwrap().clone();
        let tx_flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);

        if !is_xrp_asset(&asset) {
            // IOU/MPT vaults additionally need the pseudo-account's underlying
            // holding (RippleState/MPToken). Not yet byte-verified.
            return Err(TransactionResult::TemDisabled);
        }

        // 1. Vault keylet + pseudo-account derivation.
        let vault_key = keylet::vault(&account_id, seq);
        let pseudo_id = derive_pseudo_account(ctx, &vault_key)?;
        let pseudo_str = encode_account_id(&pseudo_id);
        let vault_id_hex = hex::encode_upper(vault_key.as_bytes());

        // 2. Pseudo-account root (Sequence 0 and Balance 0 are defaults, omitted).
        let pseudo_acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": pseudo_str,
            "Balance": "0",
            "Flags": LSF_DISABLE_MASTER | LSF_DEFAULT_RIPPLE | LSF_DEPOSIT_AUTH,
            "OwnerCount": 1,
            "VaultID": vault_id_hex,
            "PreviousTxnID": ZERO_TXID,
            "PreviousTxnLgrSeq": 0,
        });
        ctx.view
            .insert(
                keylet::account(&pseudo_id),
                serde_json::to_vec(&pseudo_acct).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // 3. Share MPTokenIssuance, issued by the pseudo-account (Sequence 1).
        let mut mpt_flags = 0u32;
        if tx_flags & TF_VAULT_SHARE_NON_TRANSFERABLE == 0 {
            mpt_flags |= LSF_MPT_CAN_ESCROW | LSF_MPT_CAN_TRADE | LSF_MPT_CAN_TRANSFER;
        }
        if tx_flags & TF_VAULT_PRIVATE != 0 {
            mpt_flags |= LSF_MPT_REQUIRE_AUTH;
        }
        let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
        let issuance = serde_json::json!({
            "LedgerEntryType": "MPTokenIssuance",
            "Flags": mpt_flags,
            "Issuer": pseudo_str,
            "Sequence": 1,
            "PreviousTxnID": ZERO_TXID,
            "PreviousTxnLgrSeq": 0,
        });
        ctx.view
            .insert(
                issuance_key,
                serde_json::to_vec(&issuance).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
        crate::owner_dir::add_to_owner_dir(ctx.view, &pseudo_id, &issuance_key)?;

        let share_id = share_mptid(&pseudo_id);

        // 4. The Vault object itself (XRP Asset is the default STIssue, omitted;
        //    AssetsTotal/AssetsAvailable/LossUnrealized default to 0, omitted).
        //    rippled links the vault into the owner directory before the share
        //    MPToken, so create it first.
        let mut vault = serde_json::json!({
            "LedgerEntryType": "Vault",
            "Account": pseudo_str,
            "Owner": account_str,
            "Sequence": seq,
            "ShareMPTID": share_id,
            "WithdrawalPolicy": VAULT_STRATEGY_FIRST_COME_FIRST_SERVE,
            "PreviousTxnID": ZERO_TXID,
            "PreviousTxnLgrSeq": 0,
        });
        if tx_flags & TF_VAULT_PRIVATE != 0 {
            vault["Flags"] = serde_json::Value::from(TF_VAULT_PRIVATE);
        }
        if let Some(max) = helpers::get_u64_str_field(ctx.tx, "AssetsMaximum") {
            vault["AssetsMaximum"] = serde_json::Value::String(max.to_string());
        }
        if let Some(data) = helpers::get_str_field(ctx.tx, "Data") {
            vault["Data"] = serde_json::Value::String(data.to_string());
        }
        ctx.view
            .insert(
                vault_key,
                serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
        crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &vault_key)?;

        // 5. Owner's MPToken holding for the shares (linked after the vault).
        let owner_mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &account_id);
        let owner_mptoken = serde_json::json!({
            "LedgerEntryType": "MPToken",
            "Account": account_str,
            "MPTokenIssuanceID": share_id,
            "PreviousTxnID": ZERO_TXID,
            "PreviousTxnLgrSeq": 0,
        });
        ctx.view
            .insert(
                owner_mptoken_key,
                serde_json::to_vec(&owner_mptoken).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
        crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &owner_mptoken_key)?;

        // 6. Owner account: bump sequence and OwnerCount by 3 (vault + pseudo +
        //    the share MPToken holding).
        helpers::increment_sequence(&mut account);
        helpers::adjust_owner_count(&mut account, 3);
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

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

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn setup_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(OWNER).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": OWNER,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn create_xrp_vault() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
            "Asset": "XRP",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify the vault object and its links.
        let owner_id = decode_account_id(OWNER).unwrap();
        let vault_key = keylet::vault(&owner_id, 1);
        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["LedgerEntryType"].as_str().unwrap(), "Vault");
        assert_eq!(vault["Owner"].as_str().unwrap(), OWNER);
        assert!(vault.get("Asset").is_none()); // XRP is the default STIssue
        assert_eq!(vault["WithdrawalPolicy"].as_u64().unwrap(), 1);
        assert_eq!(vault["Sequence"].as_u64().unwrap(), 1);

        // Pseudo-account is the vault's Account, with the share issuance.
        let pseudo_str = vault["Account"].as_str().unwrap();
        let pseudo_id = decode_account_id(pseudo_str).unwrap();
        let pseudo_bytes = sandbox.read(&keylet::account(&pseudo_id)).unwrap();
        let pseudo: serde_json::Value = serde_json::from_slice(&pseudo_bytes).unwrap();
        assert_eq!(pseudo["Flags"].as_u64().unwrap(), 26214400);
        assert_eq!(pseudo["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(
            pseudo["VaultID"].as_str().unwrap(),
            hex::encode_upper(vault_key.as_bytes())
        );

        // Share issuance is owned by the pseudo, with the default MPT flags.
        let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
        let issuance_bytes = sandbox.read(&issuance_key).unwrap();
        let issuance: serde_json::Value = serde_json::from_slice(&issuance_bytes).unwrap();
        assert_eq!(issuance["Flags"].as_u64().unwrap(), 56);
        assert_eq!(issuance["Issuer"].as_str().unwrap(), pseudo_str);
        assert_eq!(issuance["Sequence"].as_u64().unwrap(), 1);
        assert_eq!(
            vault["ShareMPTID"].as_str().unwrap(),
            share_mptid(&pseudo_id)
        );

        // Owner holds an MPToken for the shares.
        let owner_mpt_key = keylet::mptoken(issuance_key.as_bytes(), &owner_id);
        let owner_mpt_bytes = sandbox.read(&owner_mpt_key).unwrap();
        let owner_mpt: serde_json::Value = serde_json::from_slice(&owner_mpt_bytes).unwrap();
        assert_eq!(owner_mpt["Account"].as_str().unwrap(), OWNER);
        assert_eq!(
            owner_mpt["MPTokenIssuanceID"].as_str().unwrap(),
            share_mptid(&pseudo_id)
        );

        // Owner: +3 owner count (vault + pseudo + share MPToken), sequence bumped.
        let acct_bytes = sandbox.read(&keylet::account(&owner_id)).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 3);
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn create_vault_with_assets_maximum() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
            "Asset": "XRP",
            "AssetsMaximum": "50000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let owner_id = decode_account_id(OWNER).unwrap();
        let vault_key = keylet::vault(&owner_id, 1);
        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["AssetsMaximum"].as_str().unwrap(), "50000000");
    }

    #[test]
    fn reject_missing_asset() {
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
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
            VaultCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_invalid_asset_string() {
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
            "Asset": "BTC",
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
            VaultCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn accept_iou_asset() {
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
            "Asset": {
                "currency": "USD",
                "issuer": "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN"
            },
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(VaultCreateTransactor.preflight(&ctx), Ok(()));
    }
}
