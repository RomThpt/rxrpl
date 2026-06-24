use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::amm_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

const LSF_LOW_RESERVE: u64 = 0x0001_0000;
const LSF_HIGH_RESERVE: u64 = 0x0002_0000;

pub struct AMMDeleteTransactor;

impl Transactor for AMMDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let asset = ctx.tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;
        let asset2 = ctx
            .tx
            .get("Asset2")
            .ok_or(TransactionResult::TemMalformed)?;

        amm_helpers::validate_asset(asset)?;
        amm_helpers::validate_asset(asset2)?;

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let amm = amm_helpers::read_amm(ctx.view, &amm_key).map_err(|_| TransactionResult::TerNoAmm)?;

        if !lp_token_balance_is_zero(&amm) {
            return Err(TransactionResult::TecAmmNotEmpty);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let submitter =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let amm = amm_helpers::read_amm(ctx.view, &amm_key)?;
        let amm_account_str = amm["Account"]
            .as_str()
            .ok_or(TransactionResult::TecInternalError)?
            .to_string();
        let amm_account =
            decode_account_id(&amm_account_str).map_err(|_| TransactionResult::TecInternalError)?;

        delete_amm_account(ctx, &amm_key, &amm, &amm_account)?;

        bump_submitter_sequence(ctx, &submitter)?;

        Ok(TransactionResult::TesSuccess)
    }
}

fn lp_token_balance_is_zero(amm: &Value) -> bool {
    match amm.get("LPTokenBalance") {
        Some(Value::Object(o)) => o
            .get("value")
            .and_then(|v| v.as_str())
            .map(|s| amm_helpers::parse_iou_value(s).is_zero())
            .unwrap_or(true),
        Some(Value::String(s)) => s.parse::<u64>().map(|n| n == 0).unwrap_or(true),
        _ => true,
    }
}

/// Mirror of rippled `deleteAMMAccount`: erase every zero-balance pool trust
/// line, then unlink the AMM entry from the AMM account's owner dir, drop the
/// now-empty dir root, and erase the AMM entry and the AMM AccountRoot.
fn delete_amm_account(
    ctx: &mut ApplyContext<'_>,
    amm_key: &Hash256,
    amm: &Value,
    amm_account: &AccountId,
) -> Result<(), TransactionResult> {
    let amm_key_hex = amm_key.to_string().to_uppercase();
    let entries = crate::owner_dir::collect_owner_dir_entries(ctx.view, amm_account);

    for entry_hex in entries {
        if entry_hex.eq_ignore_ascii_case(&amm_key_hex) {
            continue;
        }
        let Some(entry_key) = parse_hash(&entry_hex) else {
            continue;
        };
        let Some(bytes) = ctx.view.read(&entry_key) else {
            continue;
        };
        let sle: Value = serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TecInternalError)?;
        match sle.get("LedgerEntryType").and_then(|v| v.as_str()) {
            Some("RippleState") => {
                delete_amm_trust_line(ctx, &entry_key, &sle, amm_account)?;
            }
            Some("MPToken") | Some("AMM") => {}
            _ => return Err(TransactionResult::TecInternalError),
        }
    }

    let owner_node = amm
        .get("OwnerNode")
        .and_then(|v| v.as_str())
        .and_then(|s| u64::from_str_radix(s, 16).ok())
        .unwrap_or(0);
    crate::owner_dir::remove_from_owner_dir_page(ctx.view, amm_account, owner_node, amm_key)?;

    ctx.view
        .erase(amm_key)
        .map_err(|_| TransactionResult::TecInternalError)?;

    let amm_acct_key = keylet::account(amm_account);
    ctx.view
        .erase(&amm_acct_key)
        .map_err(|_| TransactionResult::TecInternalError)?;

    Ok(())
}

/// Mirror of rippled `deleteAMMTrustLine`: remove the pool `RippleState` from
/// both owner dirs (by `LowNode`/`HighNode` hint), erase it, then decrement the
/// owner count of the NON-AMM side (the side carrying the reserve flag).
fn delete_amm_trust_line(
    ctx: &mut ApplyContext<'_>,
    line_key: &Hash256,
    line: &Value,
    amm_account: &AccountId,
) -> Result<(), TransactionResult> {
    let low_issuer = line["LowLimit"]["issuer"]
        .as_str()
        .ok_or(TransactionResult::TecInternalError)?;
    let high_issuer = line["HighLimit"]["issuer"]
        .as_str()
        .ok_or(TransactionResult::TecInternalError)?;
    let low_id = decode_account_id(low_issuer).map_err(|_| TransactionResult::TecInternalError)?;
    let high_id = decode_account_id(high_issuer).map_err(|_| TransactionResult::TecInternalError)?;
    let (low, high) = if low_id.as_bytes() <= high_id.as_bytes() {
        (low_id, high_id)
    } else {
        (high_id, low_id)
    };

    let amm_low = low == *amm_account;
    let amm_high = high == *amm_account;
    if amm_low == amm_high {
        return Err(TransactionResult::TecInternalError);
    }

    let node_of = |field: &str| -> u64 {
        line.get(field)
            .and_then(|v| v.as_str())
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0)
    };
    let low_node = node_of("LowNode");
    let high_node = node_of("HighNode");

    crate::owner_dir::remove_from_owner_dir_page(ctx.view, &low, low_node, line_key)?;
    crate::owner_dir::remove_from_owner_dir_page(ctx.view, &high, high_node, line_key)?;
    ctx.view
        .erase(line_key)
        .map_err(|_| TransactionResult::TecInternalError)?;

    let flags = line.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0);
    let reserve_flag = if amm_low {
        LSF_HIGH_RESERVE
    } else {
        LSF_LOW_RESERVE
    };
    if flags & reserve_flag == 0 {
        return Err(TransactionResult::TecInternalError);
    }

    let non_amm = if amm_low { &high } else { &low };
    adjust_owner_count(ctx, non_amm, -1)
}

fn adjust_owner_count(
    ctx: &mut ApplyContext<'_>,
    account: &AccountId,
    delta: i32,
) -> Result<(), TransactionResult> {
    let key = keylet::account(account);
    let bytes = ctx.view.read(&key).ok_or(TransactionResult::TecInternalError)?;
    let mut acct: Value = serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TecInternalError)?;
    helpers::adjust_owner_count(&mut acct, delta);
    let data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TecInternalError)?;
    ctx.view
        .update(key, data)
        .map_err(|_| TransactionResult::TecInternalError)
}

fn bump_submitter_sequence(
    ctx: &mut ApplyContext<'_>,
    submitter: &AccountId,
) -> Result<(), TransactionResult> {
    let key = keylet::account(submitter);
    let bytes = ctx.view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
    let mut acct: Value = serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    helpers::increment_sequence(&mut acct);
    let data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(key, data)
        .map_err(|_| TransactionResult::TefInternal)
}

fn parse_hash(hex_str: &str) -> Option<Hash256> {
    let bytes = hex::decode(hex_str).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Hash256::from(arr))
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
    use rxrpl_codec::address::classic::encode_account_id;
    use rxrpl_ledger::Ledger;

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const BOB: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn put(ledger: &mut Ledger, key: Hash256, obj: &Value) {
        ledger
            .put_state(key, serde_json::to_vec(obj).unwrap())
            .unwrap();
    }

    fn dir_with(root: Hash256, entries: &[Hash256], owner: Option<&str>) -> Value {
        let idx: Vec<String> = entries
            .iter()
            .map(|h| h.to_string().to_uppercase())
            .collect();
        let mut o = serde_json::json!({
            "LedgerEntryType": "DirectoryNode",
            "Flags": 0,
            "RootIndex": root.to_string().to_uppercase(),
            "Indexes": idx,
        });
        if let Some(owner) = owner {
            o["Owner"] = Value::String(owner.to_string());
        }
        o
    }

    /// Build an empty XRP/USD AMM owned by a pseudo-account, with a single pool
    /// trust line AMM<->BOB (USD issuer). The reserve flag and owner count sit on
    /// BOB (the non-AMM side). Returns (ledger, amm_key, amm_account, lp_balance_zero?).
    fn setup_empty_amm(lp_value: &str) -> (Ledger, Hash256, AccountId) {
        let mut ledger = Ledger::genesis();

        let alice = decode_account_id(ALICE).unwrap();
        let bob = decode_account_id(BOB).unwrap();

        let asset = serde_json::json!("XRP");
        let asset2 = serde_json::json!({"currency": "USD", "issuer": BOB});
        let amm_key = amm_helpers::compute_amm_key(&asset, &asset2).unwrap();
        let amm_account = amm_helpers::amm_pseudo_account(&amm_key);
        let amm_str = encode_account_id(&amm_account);

        // Submitter (ALICE).
        put(
            &mut ledger,
            keylet::account(&alice),
            &serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": ALICE,
                "Balance": "100000000",
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            }),
        );

        // BOB: USD issuer, holds the reserve for the pool line.
        put(
            &mut ledger,
            keylet::account(&bob),
            &serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": BOB,
                "Balance": "100000000",
                "Sequence": 1,
                "OwnerCount": 1,
                "Flags": 0,
            }),
        );

        // AMM pseudo-account.
        put(
            &mut ledger,
            keylet::account(&amm_account),
            &serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": amm_str,
                "Balance": "0",
                "Sequence": 1,
                "OwnerCount": 1,
                "Flags": 0,
                "AMMID": hex::encode_upper(amm_key.as_bytes()),
            }),
        );

        // Pool trust line AMM<->BOB for USD. Reserve flag on the NON-AMM side.
        let mut usd = [0u8; 20];
        usd[12..15].copy_from_slice(b"USD");
        let tl_key = keylet::trust_line(&amm_account, &bob, &usd);
        let amm_is_low = amm_account.as_bytes() < bob.as_bytes();
        let reserve_flag: u64 = if amm_is_low {
            LSF_HIGH_RESERVE
        } else {
            LSF_LOW_RESERVE
        };
        let (low_str, high_str) = if amm_is_low {
            (amm_str.clone(), BOB.to_string())
        } else {
            (BOB.to_string(), amm_str.clone())
        };
        put(
            &mut ledger,
            tl_key,
            &serde_json::json!({
                "LedgerEntryType": "RippleState",
                "Balance": {"currency": "USD", "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": "0"},
                "LowLimit": {"currency": "USD", "issuer": low_str, "value": "0"},
                "HighLimit": {"currency": "USD", "issuer": high_str, "value": "0"},
                "Flags": reserve_flag,
                "LowNode": "0000000000000000",
                "HighNode": "0000000000000000",
            }),
        );

        // AMM entry (empty: LPTokenBalance value == lp_value).
        let lp_currency = amm_helpers::lp_currency_hex(&amm_key);
        put(
            &mut ledger,
            amm_key,
            &serde_json::json!({
                "LedgerEntryType": "AMM",
                "Account": amm_str,
                "Asset2": {"currency": "USD", "issuer": BOB},
                "LPTokenBalance": {"currency": lp_currency, "issuer": amm_str, "value": lp_value},
                "TradingFee": 0,
                "OwnerNode": "0000000000000000",
            }),
        );

        // AMM account's owner dir: holds the AMM entry + the pool line.
        let amm_owner_root = keylet::owner_dir(&amm_account);
        put(
            &mut ledger,
            amm_owner_root,
            &dir_with(amm_owner_root, &[amm_key, tl_key], Some(&amm_str)),
        );

        // BOB's owner dir: holds the pool line.
        let bob_owner_root = keylet::owner_dir(&bob);
        put(
            &mut ledger,
            bob_owner_root,
            &dir_with(bob_owner_root, &[tl_key], Some(BOB)),
        );

        (ledger, amm_key, amm_account)
    }

    fn delete_tx() -> Value {
        serde_json::json!({
            "TransactionType": "AMMDelete",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Fee": "12",
            "Sequence": 1,
        })
    }

    #[test]
    fn delete_empty_amm_erases_pool_and_account() {
        let (ledger, amm_key, amm_account) = setup_empty_amm("0");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = delete_tx();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            AMMDeleteTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        let bob = decode_account_id(BOB).unwrap();
        let alice = decode_account_id(ALICE).unwrap();
        let mut usd = [0u8; 20];
        usd[12..15].copy_from_slice(b"USD");
        let tl_key = keylet::trust_line(&amm_account, &bob, &usd);

        assert!(!sandbox.exists(&amm_key), "AMM entry erased");
        assert!(
            !sandbox.exists(&keylet::account(&amm_account)),
            "AMM account erased"
        );
        assert!(!sandbox.exists(&tl_key), "pool trust line erased");
        assert!(
            !sandbox.exists(&keylet::owner_dir(&amm_account)),
            "empty AMM owner dir root erased"
        );

        let bob_acct: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&bob)).unwrap()).unwrap();
        assert_eq!(
            bob_acct["OwnerCount"].as_u64().unwrap(),
            0,
            "non-AMM owner count decremented"
        );

        // BOB's owner dir is now empty -> root erased.
        assert!(!sandbox.exists(&keylet::owner_dir(&bob)));

        let alice_acct: Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&alice)).unwrap()).unwrap();
        assert_eq!(alice_acct["Sequence"].as_u64().unwrap(), 2);
        assert_eq!(
            alice_acct["OwnerCount"].as_u64().unwrap(),
            0,
            "submitter owner count untouched"
        );
    }

    #[test]
    fn preclaim_rejects_nonempty_lp_balance() {
        let (ledger, _amm_key, _amm_account) = setup_empty_amm("1000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = delete_tx();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AMMDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecAmmNotEmpty)
        );
    }

    #[test]
    fn preclaim_accepts_empty_lp_balance() {
        let (ledger, _amm_key, _amm_account) = setup_empty_amm("0");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = delete_tx();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(AMMDeleteTransactor.preclaim(&ctx), Ok(()));
    }

    #[test]
    fn preclaim_rejects_missing_amm() {
        let mut ledger = Ledger::genesis();
        let alice = decode_account_id(ALICE).unwrap();
        put(
            &mut ledger,
            keylet::account(&alice),
            &serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": ALICE,
                "Balance": "100000000",
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            }),
        );
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = delete_tx();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AMMDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TerNoAmm)
        );
    }

    #[test]
    fn reject_missing_asset() {
        let tx = serde_json::json!({
            "TransactionType": "AMMDelete",
            "Account": ALICE,
            "Asset": "XRP",
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
            AMMDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }
}
