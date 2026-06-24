use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::owner_dir::{
    collect_owner_dir_entries, dir_remove, remove_from_owner_dir, remove_from_owner_dir_page,
};
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// AccountDelete transaction handler.
///
/// Deletes an account and transfers remaining XRP to a destination. Any
/// directly-deletable objects it owns are removed first; obligation-bearing
/// objects block deletion. Charges an elevated fee (5x reserve increment).
pub struct AccountDeleteTransactor;

/// Ledger flag: destination requires deposit authorization.
const LSF_DEPOSIT_AUTH: u32 = 0x01000000;

/// Ledger flag: destination requires a destination tag.
const LSF_REQUIRE_DEST_TAG: u32 = 0x00020000;

impl Transactor for AccountDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Destination must be present
        let destination = helpers::get_destination(ctx.tx)?;

        // Account must not equal Destination
        let account = helpers::get_account(ctx.tx)?;
        if account == destination {
            return Err(TransactionResult::TemBadSend);
        }

        Ok(())
    }

    fn calculate_base_fee(&self, ctx: &PreflightContext<'_>) -> u64 {
        // AccountDelete costs 5x the owner reserve increment (default 10 XRP)
        ctx.fees.reserve_increment * 5
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let destination_str = helpers::get_destination(ctx.tx)?;

        // Source must exist
        let src_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let src_key = keylet::account(&src_id);
        if !ctx.view.exists(&src_key) {
            return Err(TransactionResult::TerNoAccount);
        }

        // The account may only be deleted if every object it owns is a
        // directly-deletable (non-obligation) type. Anything else (trust lines,
        // escrows, checks, paychannels, NFToken pages, ...) is an obligation
        // that blocks deletion. Mirrors rippled's DeleteAccount deleter map.
        for entry_hex in collect_owner_dir_entries(ctx.view, &src_id) {
            let Some(entry_key) = parse_hash(&entry_hex) else {
                continue;
            };
            let Some(bytes) = ctx.view.read(&entry_key) else {
                continue;
            };
            let sle: Value =
                serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
            if !is_deletable_type(sle.get("LedgerEntryType").and_then(|v| v.as_str())) {
                return Err(TransactionResult::TecHasObligations);
            }
        }

        // Destination must exist
        let dst_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = ctx.view.read(&dst_key).ok_or(TransactionResult::TecNoDst)?;
        let dst_obj: Value =
            serde_json::from_slice(&dst_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // If destination has deposit auth, check for preauthorization
        let dst_flags = helpers::get_flags(&dst_obj);
        if dst_flags & LSF_DEPOSIT_AUTH != 0 {
            let preauth_key = keylet::deposit_preauth(&dst_id, &src_id);
            if !ctx.view.exists(&preauth_key) {
                return Err(TransactionResult::TecNoPermission);
            }
        }

        // If destination requires dest tag, check DestinationTag is present
        if dst_flags & LSF_REQUIRE_DEST_TAG != 0
            && helpers::get_u32_field(ctx.tx, "DestinationTag").is_none()
        {
            return Err(TransactionResult::TecDstTagNeeded);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let destination_str = helpers::get_destination(ctx.tx)?;

        let src_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let dst_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let src_key = keylet::account(&src_id);
        let dst_key = keylet::account(&dst_id);

        // Delete every directly-deletable object the account owns (offers,
        // tickets, signer lists, NFToken offers, DIDs, deposit preauths) before
        // removing the account itself. The TicketSequence the tx consumes is one
        // such owned Ticket and is erased here too. preclaim has already proven
        // no obligation-bearing objects remain.
        for entry_hex in collect_owner_dir_entries(ctx.view, &src_id) {
            let Some(entry_key) = parse_hash(&entry_hex) else {
                continue;
            };
            let Some(bytes) = ctx.view.read(&entry_key) else {
                continue;
            };
            let sle: Value =
                serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
            delete_owned_object(ctx, &src_id, &entry_key, &sle)?;
        }

        // Read source account
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut src_obj: Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Get remaining balance (fee already deducted by engine)
        let remaining = helpers::get_balance(&src_obj);

        // Transfer remaining balance to destination
        if remaining > 0 {
            let dst_bytes = ctx.view.read(&dst_key).ok_or(TransactionResult::TecNoDst)?;
            let mut dst_obj: Value =
                serde_json::from_slice(&dst_bytes).map_err(|_| TransactionResult::TefInternal)?;

            let dst_balance = helpers::get_balance(&dst_obj);
            let new_dst_balance = dst_balance
                .checked_add(remaining)
                .ok_or(TransactionResult::TefInternal)?;
            helpers::set_balance(&mut dst_obj, new_dst_balance);

            let dst_data =
                serde_json::to_vec(&dst_obj).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(dst_key, dst_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Zero out source balance and delete
        helpers::set_balance(&mut src_obj, 0);
        // Set OwnerCount to 0 explicitly for invariant check
        src_obj["OwnerCount"] = Value::from(0u64);

        // We need to update before erase so the deleted data has balance==0
        let src_data = serde_json::to_vec(&src_obj).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Erase the account
        ctx.view
            .erase(&src_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

/// Ledger entry types rippled deletes automatically as part of AccountDelete.
/// Every other owned type is an obligation that blocks deletion.
fn is_deletable_type(ty: Option<&str>) -> bool {
    matches!(
        ty,
        Some("Offer")
            | Some("SignerList")
            | Some("Ticket")
            | Some("NFTokenOffer")
            | Some("DID")
            | Some("DepositPreauth")
    )
}

fn parse_hash(hex_str: &str) -> Option<Hash256> {
    let bytes = hex::decode(hex_str).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(Hash256::from(arr))
}

fn node_hint(sle: &Value, field: &str) -> u64 {
    sle.get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| u64::from_str_radix(s, 16).ok())
        .unwrap_or(0)
}

/// Unlink an owned object from its directories and erase it. Mirrors the
/// per-type deleters rippled invokes from DeleteAccount. The owning account is
/// being erased, so its OwnerCount is not adjusted here.
fn delete_owned_object(
    ctx: &mut ApplyContext<'_>,
    owner_id: &AccountId,
    key: &Hash256,
    sle: &Value,
) -> Result<(), TransactionResult> {
    match sle.get("LedgerEntryType").and_then(|v| v.as_str()) {
        Some("NFTokenOffer") => {
            remove_from_owner_dir_page(ctx.view, owner_id, node_hint(sle, "OwnerNode"), key)?;
            let is_sell = sle.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & 1 != 0;
            if let Some(nft_hash) = sle
                .get("NFTokenID")
                .and_then(|v| v.as_str())
                .and_then(parse_hash)
            {
                let book = if is_sell {
                    keylet::nft_sells(&nft_hash)
                } else {
                    keylet::nft_buys(&nft_hash)
                };
                dir_remove(ctx.view, &book, key)?;
            }
            // A destination-restricted offer threads its Destination's
            // PreviousTxnID even though no field changes.
            if let Some(dest_id) = sle
                .get("Destination")
                .and_then(|v| v.as_str())
                .and_then(|d| decode_account_id(d).ok())
            {
                let dest_key = keylet::account(&dest_id);
                if let Some(dest_bytes) = ctx.view.read(&dest_key) {
                    ctx.view
                        .update(dest_key, dest_bytes)
                        .map_err(|_| TransactionResult::TefInternal)?;
                }
            }
        }
        Some("Offer") => {
            if let Some(book) = sle
                .get("BookDirectory")
                .and_then(|v| v.as_str())
                .and_then(parse_hash)
            {
                dir_remove(ctx.view, &book, key)?;
            }
            remove_from_owner_dir(ctx.view, owner_id, key)?;
        }
        Some("SignerList") | Some("Ticket") | Some("DID") | Some("DepositPreauth") => {
            remove_from_owner_dir(ctx.view, owner_id, key)?;
        }
        _ => return Err(TransactionResult::TefInternal),
    }
    ctx.view
        .erase(key)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
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

    const SRC_ADDRESS: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const DST_ADDRESS: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_ledger_with_account(address: &str, balance: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let data = serde_json::to_vec(&account).unwrap();
        ledger.put_state(key, data).unwrap();
        ledger
    }

    fn add_account(ledger: &mut Ledger, address: &str, balance: u64) {
        add_account_with_flags(ledger, address, balance, 0);
    }

    fn add_account_with_flags(ledger: &mut Ledger, address: &str, balance: u64, flags: u32) {
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": flags,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
    }

    fn make_account_delete_tx(account: &str, destination: &str) -> Value {
        serde_json::json!({
            "TransactionType": "AccountDelete",
            "Account": account,
            "Destination": destination,
            "Fee": "10000000",
        })
    }

    // -- preflight tests --

    #[test]
    fn preflight_missing_destination() {
        let tx = serde_json::json!({
            "TransactionType": "AccountDelete",
            "Account": SRC_ADDRESS,
            "Fee": "10000000",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            AccountDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemDstIsObligatory)
        );
    }

    #[test]
    fn preflight_self_delete() {
        let tx = make_account_delete_tx(SRC_ADDRESS, SRC_ADDRESS);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            AccountDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadSend)
        );
    }

    #[test]
    fn preflight_valid() {
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert!(AccountDeleteTransactor.preflight(&ctx).is_ok());
    }

    #[test]
    fn calculate_base_fee_is_5x_increment() {
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        // 2_000_000 * 5 = 10_000_000 (10 XRP)
        assert_eq!(AccountDeleteTransactor.calculate_base_fee(&ctx), 10_000_000);
    }

    // -- preclaim tests --

    #[test]
    fn preclaim_source_not_found() {
        let mut ledger = Ledger::genesis();
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AccountDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TerNoAccount)
        );
    }

    #[test]
    fn preclaim_has_obligations() {
        let mut ledger = Ledger::genesis();
        // Source owning a non-deletable object (an Escrow) linked into its owner
        // directory: that is an obligation that blocks deletion.
        let src_id = decode_account_id(SRC_ADDRESS).unwrap();
        let src_key = keylet::account(&src_id);
        let src = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": SRC_ADDRESS,
            "Balance": "10000000",
            "Sequence": 1,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(src_key, serde_json::to_vec(&src).unwrap())
            .unwrap();

        let escrow_key = keylet::escrow(&src_id, 1);
        let escrow = serde_json::json!({
            "LedgerEntryType": "Escrow",
            "Account": SRC_ADDRESS,
            "Destination": DST_ADDRESS,
            "Amount": "1000000",
        });
        ledger
            .put_state(escrow_key, serde_json::to_vec(&escrow).unwrap())
            .unwrap();

        let dir_root = keylet::owner_dir(&src_id);
        let dir_page0 = keylet::dir_node(&dir_root, 0);
        let dir = serde_json::json!({
            "LedgerEntryType": "DirectoryNode",
            "Owner": SRC_ADDRESS,
            "Indexes": [escrow_key.to_string().to_uppercase()],
            "IndexNext": "0",
            "IndexPrevious": "0",
        });
        ledger
            .put_state(dir_page0, serde_json::to_vec(&dir).unwrap())
            .unwrap();
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AccountDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecHasObligations)
        );
    }

    #[test]
    fn preclaim_destination_not_found() {
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AccountDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoDst)
        );
    }

    #[test]
    fn preclaim_deposit_auth_no_preauth() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account_with_flags(&mut ledger, DST_ADDRESS, 5_000_000, LSF_DEPOSIT_AUTH);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AccountDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn preclaim_require_dest_tag_missing() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account_with_flags(&mut ledger, DST_ADDRESS, 5_000_000, LSF_REQUIRE_DEST_TAG);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AccountDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecDstTagNeeded)
        );
    }

    #[test]
    fn preclaim_require_dest_tag_present() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account_with_flags(&mut ledger, DST_ADDRESS, 5_000_000, LSF_REQUIRE_DEST_TAG);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        tx["DestinationTag"] = Value::from(42);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert!(AccountDeleteTransactor.preclaim(&ctx).is_ok());
    }

    #[test]
    fn preclaim_valid() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert!(AccountDeleteTransactor.preclaim(&ctx).is_ok());
    }

    // -- apply tests --

    #[test]
    fn apply_deletes_account_and_transfers_balance() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AccountDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Source should be erased
        let src_id = decode_account_id(SRC_ADDRESS).unwrap();
        let src_key = keylet::account(&src_id);
        assert!(sandbox.read(&src_key).is_none());

        // Destination should have received the balance
        let dst_id = decode_account_id(DST_ADDRESS).unwrap();
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = sandbox.read(&dst_key).unwrap();
        let dst: Value = serde_json::from_slice(&dst_bytes).unwrap();
        assert_eq!(dst["Balance"].as_str().unwrap(), "15000000");
    }

    #[test]
    fn apply_source_not_found() {
        let mut ledger = Ledger::genesis();
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AccountDeleteTransactor.apply(&mut ctx);
        assert_eq!(result, Err(TransactionResult::TerNoAccount));
    }

    #[test]
    fn apply_deletes_owned_nftoken_offer() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let src_id = decode_account_id(SRC_ADDRESS).unwrap();
        let offer_key = keylet::nftoken_offer(&src_id, 1);
        let nft_id = "00080000A1B2C3D4E5F60708090A0B0C0D0E0F1011121314000003E800000001";
        let offer = serde_json::json!({
            "LedgerEntryType": "NFTokenOffer",
            "Owner": SRC_ADDRESS,
            "NFTokenID": nft_id,
            "Amount": "1000000",
            "Flags": 1,
            "OwnerNode": "0",
        });
        ledger
            .put_state(offer_key, serde_json::to_vec(&offer).unwrap())
            .unwrap();

        let dir_root = keylet::owner_dir(&src_id);
        let dir = serde_json::json!({
            "LedgerEntryType": "DirectoryNode",
            "Owner": SRC_ADDRESS,
            "Indexes": [offer_key.to_string().to_uppercase()],
            "IndexNext": "0",
            "IndexPrevious": "0",
        });
        ledger
            .put_state(
                keylet::dir_node(&dir_root, 0),
                serde_json::to_vec(&dir).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AccountDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
        assert!(sandbox.read(&offer_key).is_none());
        assert!(sandbox.read(&keylet::account(&src_id)).is_none());
    }
}
