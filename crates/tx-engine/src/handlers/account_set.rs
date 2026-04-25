use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// AccountSet flag constants.
const ASF_REQUIRE_DEST: u32 = 1;
const ASF_REQUIRE_AUTH: u32 = 2;
const ASF_DISALLOW_XRP: u32 = 3;
const ASF_DISABLE_MASTER: u32 = 4;
const ASF_NO_FREEZE: u32 = 6;
const ASF_GLOBAL_FREEZE: u32 = 7;
const ASF_DEFAULT_RIPPLE: u32 = 8;
const ASF_DEPOSIT_AUTH: u32 = 9;
const ASF_AUTHORIZED_NFTOKEN_MINTER: u32 = 10;
const ASF_DISALLOW_INCOMING_NFTOKEN_OFFER: u32 = 12;
const ASF_DISALLOW_INCOMING_CHECK: u32 = 13;
const ASF_DISALLOW_INCOMING_PAY_CHAN: u32 = 14;
const ASF_DISALLOW_INCOMING_TRUSTLINE: u32 = 15;
const ASF_ALLOW_TRUST_LINE_CLAWBACK: u32 = 16;

/// Ledger flags corresponding to account set flags.
const LSF_REQUIRE_DEST_TAG: u32 = 0x00020000;
const LSF_REQUIRE_AUTH: u32 = 0x00040000;
const LSF_DISALLOW_XRP: u32 = 0x00080000;
const LSF_DISABLE_MASTER: u32 = 0x00100000;
const LSF_NO_FREEZE: u32 = 0x00200000;
const LSF_GLOBAL_FREEZE: u32 = 0x00400000;
const LSF_DEFAULT_RIPPLE: u32 = 0x00800000;
const LSF_DEPOSIT_AUTH: u32 = 0x01000000;
const LSF_DISALLOW_INCOMING_NFTOKEN_OFFER: u32 = 0x04000000;
const LSF_DISALLOW_INCOMING_CHECK: u32 = 0x08000000;
const LSF_DISALLOW_INCOMING_PAY_CHAN: u32 = 0x10000000;
const LSF_DISALLOW_INCOMING_TRUSTLINE: u32 = 0x20000000;
const LSF_ALLOW_TRUST_LINE_CLAWBACK: u32 = 0x80000000;

fn asf_to_lsf(asf: u32) -> Option<u32> {
    match asf {
        ASF_REQUIRE_DEST => Some(LSF_REQUIRE_DEST_TAG),
        ASF_REQUIRE_AUTH => Some(LSF_REQUIRE_AUTH),
        ASF_DISALLOW_XRP => Some(LSF_DISALLOW_XRP),
        ASF_DISABLE_MASTER => Some(LSF_DISABLE_MASTER),
        ASF_NO_FREEZE => Some(LSF_NO_FREEZE),
        ASF_GLOBAL_FREEZE => Some(LSF_GLOBAL_FREEZE),
        ASF_DEFAULT_RIPPLE => Some(LSF_DEFAULT_RIPPLE),
        ASF_DEPOSIT_AUTH => Some(LSF_DEPOSIT_AUTH),
        ASF_DISALLOW_INCOMING_NFTOKEN_OFFER => Some(LSF_DISALLOW_INCOMING_NFTOKEN_OFFER),
        ASF_DISALLOW_INCOMING_CHECK => Some(LSF_DISALLOW_INCOMING_CHECK),
        ASF_DISALLOW_INCOMING_PAY_CHAN => Some(LSF_DISALLOW_INCOMING_PAY_CHAN),
        ASF_DISALLOW_INCOMING_TRUSTLINE => Some(LSF_DISALLOW_INCOMING_TRUSTLINE),
        ASF_ALLOW_TRUST_LINE_CLAWBACK => Some(LSF_ALLOW_TRUST_LINE_CLAWBACK),
        _ => None,
    }
}

/// AccountSet transaction handler.
///
/// Modifies account flags and settings (domain, email hash,
/// message key, transfer rate, tick size, NFToken minter).
pub struct AccountSetTransactor;

impl Transactor for AccountSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let set_flag = ctx.tx.get("SetFlag").and_then(|v| v.as_u64());
        let clear_flag = ctx.tx.get("ClearFlag").and_then(|v| v.as_u64());

        // Cannot set and clear the same flag
        if let (Some(sf), Some(cf)) = (set_flag, clear_flag) {
            if sf == cf {
                return Err(TransactionResult::TemInvalidFlag);
            }
        }

        // Validate transfer rate if present
        if let Some(rate) = ctx.tx.get("TransferRate").and_then(|v| v.as_u64()) {
            if rate != 0 && !(1_000_000_000..=2_000_000_000).contains(&rate) {
                return Err(TransactionResult::TemBadTransferRate);
            }
        }

        // Validate tick size if present
        if let Some(tick) = ctx.tx.get("TickSize").and_then(|v| v.as_u64()) {
            if tick != 0 && !(3..=15).contains(&tick) {
                return Err(TransactionResult::TemBadTickSize);
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

        // Cannot set NoFreeze if GlobalFreeze is already set
        let set_flag = ctx
            .tx
            .get("SetFlag")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        if set_flag == Some(ASF_NO_FREEZE) {
            if let Some(bytes) = ctx.view.read(&key) {
                if let Ok(obj) = serde_json::from_slice::<Value>(&bytes) {
                    let flags = obj["Flags"].as_u64().unwrap_or(0) as u32;
                    if flags & LSF_GLOBAL_FREEZE != 0 {
                        return Err(TransactionResult::TecNoPermission);
                    }
                }
            }
        }

        // Cannot clear NoFreeze once set
        let clear_flag = ctx
            .tx
            .get("ClearFlag")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        if clear_flag == Some(ASF_NO_FREEZE) {
            return Err(TransactionResult::TecNoPermission);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let key = keylet::account(&account_id);

        let bytes = ctx.view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
        let mut obj: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;

        let mut flags = obj["Flags"].as_u64().unwrap_or(0) as u32;

        // Apply SetFlag
        if let Some(asf) = ctx
            .tx
            .get("SetFlag")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
        {
            if let Some(lsf) = asf_to_lsf(asf) {
                flags |= lsf;
            }
        }

        // Apply ClearFlag
        if let Some(asf) = ctx
            .tx
            .get("ClearFlag")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
        {
            if let Some(lsf) = asf_to_lsf(asf) {
                flags &= !lsf;
            }
        }

        obj["Flags"] = Value::from(flags);

        // Apply Domain
        if let Some(domain) = ctx.tx.get("Domain") {
            if domain.as_str().is_some_and(|s| s.is_empty()) {
                obj.as_object_mut().unwrap().remove("Domain");
            } else {
                obj["Domain"] = domain.clone();
            }
        }

        // Apply EmailHash
        if let Some(email) = ctx.tx.get("EmailHash") {
            if email.as_str().is_some_and(|s| s.is_empty()) {
                obj.as_object_mut().unwrap().remove("EmailHash");
            } else {
                obj["EmailHash"] = email.clone();
            }
        }

        // Apply MessageKey
        if let Some(mk) = ctx.tx.get("MessageKey") {
            if mk.as_str().is_some_and(|s| s.is_empty()) {
                obj.as_object_mut().unwrap().remove("MessageKey");
            } else {
                obj["MessageKey"] = mk.clone();
            }
        }

        // Apply TransferRate
        if let Some(rate) = ctx.tx.get("TransferRate").and_then(|v| v.as_u64()) {
            if rate == 0 || rate == 1_000_000_000 {
                obj.as_object_mut().unwrap().remove("TransferRate");
            } else {
                obj["TransferRate"] = Value::from(rate);
            }
        }

        // Apply TickSize
        if let Some(tick) = ctx.tx.get("TickSize").and_then(|v| v.as_u64()) {
            if tick == 0 {
                obj.as_object_mut().unwrap().remove("TickSize");
            } else {
                obj["TickSize"] = Value::from(tick);
            }
        }

        // Apply NFTokenMinter
        if let Some(asf) = ctx
            .tx
            .get("SetFlag")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
        {
            if asf == ASF_AUTHORIZED_NFTOKEN_MINTER {
                if let Some(minter) = ctx.tx.get("NFTokenMinter") {
                    obj["NFTokenMinter"] = minter.clone();
                }
            }
        }
        if let Some(asf) = ctx
            .tx
            .get("ClearFlag")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
        {
            if asf == ASF_AUTHORIZED_NFTOKEN_MINTER {
                obj.as_object_mut().unwrap().remove("NFTokenMinter");
            }
        }

        // Increment sequence
        helpers::increment_sequence(&mut obj);

        let new_bytes = serde_json::to_vec(&obj).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .update(key, new_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    fn setup_account(ledger: &mut Ledger, address: &str) -> rxrpl_primitives::Hash256 {
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let obj = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": "100000000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&obj).unwrap())
            .unwrap();
        key
    }

    #[test]
    fn set_require_dest_flag() {
        let mut ledger = Ledger::genesis();
        let addr = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
        let key = setup_account(&mut ledger, addr);

        let tx = serde_json::json!({
            "TransactionType": "AccountSet",
            "Account": addr,
            "Fee": "10",
            "SetFlag": ASF_REQUIRE_DEST,
        });

        let transactor = AccountSetTransactor;
        let fees = crate::fees::FeeSettings::default();
        let rules = Rules::new();

        let pf_ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        transactor.preflight(&pf_ctx).unwrap();

        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        let mut apply_ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = transactor.apply(&mut apply_ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify flag was set
        let updated = sandbox.read(&key).unwrap();
        let obj: Value = serde_json::from_slice(&updated).unwrap();
        let flags = obj["Flags"].as_u64().unwrap() as u32;
        assert!(flags & LSF_REQUIRE_DEST_TAG != 0);
    }

    #[test]
    fn clear_flag() {
        let mut ledger = Ledger::genesis();
        let addr = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
        let account_id = decode_account_id(addr).unwrap();
        let key = keylet::account(&account_id);
        let obj = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": addr,
            "Balance": "100000000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": LSF_REQUIRE_DEST_TAG,
        });
        ledger
            .put_state(key, serde_json::to_vec(&obj).unwrap())
            .unwrap();

        let tx = serde_json::json!({
            "TransactionType": "AccountSet",
            "Account": addr,
            "Fee": "10",
            "ClearFlag": ASF_REQUIRE_DEST,
        });

        let transactor = AccountSetTransactor;
        let fees = crate::fees::FeeSettings::default();
        let rules = Rules::new();
        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        let mut apply_ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = transactor.apply(&mut apply_ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let updated = sandbox.read(&key).unwrap();
        let obj: Value = serde_json::from_slice(&updated).unwrap();
        let flags = obj["Flags"].as_u64().unwrap() as u32;
        assert!(flags & LSF_REQUIRE_DEST_TAG == 0);
    }

    #[test]
    fn invalid_transfer_rate() {
        let tx = serde_json::json!({
            "TransactionType": "AccountSet",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Fee": "10",
            "TransferRate": 500000000,
        });
        let transactor = AccountSetTransactor;
        let fees = crate::fees::FeeSettings::default();
        let rules = Rules::new();
        let pf_ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            transactor.preflight(&pf_ctx),
            Err(TransactionResult::TemBadTransferRate)
        );
    }

    #[test]
    fn set_and_clear_same_flag_fails() {
        let tx = serde_json::json!({
            "TransactionType": "AccountSet",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Fee": "10",
            "SetFlag": ASF_REQUIRE_DEST,
            "ClearFlag": ASF_REQUIRE_DEST,
        });
        let transactor = AccountSetTransactor;
        let fees = crate::fees::FeeSettings::default();
        let rules = Rules::new();
        let pf_ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            transactor.preflight(&pf_ctx),
            Err(TransactionResult::TemInvalidFlag)
        );
    }
}
