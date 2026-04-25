//! Owner directory maintenance helpers.
//!
//! Each `AccountRoot` references a `DirectoryNode` page list keyed at
//! `owner_dir(account)`. Per-owner ledger objects (Check, Escrow, Offer,
//! PayChannel, etc.) must be linked into this directory so that
//! `account_objects` and friends can enumerate them.
//!
//! This implementation supports a single root page (≤31 entries — well
//! aligned with rippled's per-page split threshold). Filling the page
//! returns `TecDirFull`; a multi-page implementation can be layered on
//! top later without changing call-sites.

use rxrpl_codec::address::classic::encode_account_id;
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::view::apply_view::ApplyView;

/// Maximum entries kept in the root page before we'd need to split.
/// rippled's `dirNodeMaxEntries` is 32; we cap at 31 to leave headroom.
const MAX_ENTRIES_PER_PAGE: usize = 31;

/// Add an entry's hash to the account's owner directory.
///
/// Creates the root page if it doesn't exist yet. If the entry is already
/// listed (idempotency), returns `Ok(())` without modification.
pub fn add_to_owner_dir(
    view: &mut dyn ApplyView,
    account_id: &AccountId,
    entry_key: &Hash256,
) -> Result<(), TransactionResult> {
    let root_key = keylet::owner_dir(account_id);
    let entry_hex = entry_key.to_string();

    match view.read(&root_key) {
        None => {
            // Root page absent → create.
            let dir = serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Owner": encode_account_id(account_id),
                "RootIndex": root_key.to_string(),
                "Indexes": [entry_hex],
                "Flags": 0,
            });
            let bytes =
                serde_json::to_vec(&dir).map_err(|_| TransactionResult::TefInternal)?;
            view.insert(root_key, bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
        }
        Some(bytes) => {
            let mut dir: Value =
                serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
            let indexes = dir
                .get_mut("Indexes")
                .and_then(|v| v.as_array_mut())
                .ok_or(TransactionResult::TefInternal)?;

            if indexes.iter().any(|v| v.as_str() == Some(entry_hex.as_str())) {
                return Ok(());
            }
            if indexes.len() >= MAX_ENTRIES_PER_PAGE {
                return Err(TransactionResult::TecDirFull);
            }
            indexes.push(Value::String(entry_hex));

            let new_bytes =
                serde_json::to_vec(&dir).map_err(|_| TransactionResult::TefInternal)?;
            view.update(root_key, new_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
        }
    }
    Ok(())
}

/// Remove an entry's hash from the account's owner directory.
///
/// Erases the root page if it becomes empty. Removing a non-existent
/// entry is a no-op (returns `Ok(())`) — defensive parity with rippled
/// which tolerates redundant unlinks during cleanup paths.
pub fn remove_from_owner_dir(
    view: &mut dyn ApplyView,
    account_id: &AccountId,
    entry_key: &Hash256,
) -> Result<(), TransactionResult> {
    let root_key = keylet::owner_dir(account_id);
    let entry_hex = entry_key.to_string();

    let bytes = match view.read(&root_key) {
        Some(b) => b,
        None => return Ok(()),
    };

    let mut dir: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let indexes = dir
        .get_mut("Indexes")
        .and_then(|v| v.as_array_mut())
        .ok_or(TransactionResult::TefInternal)?;

    let original_len = indexes.len();
    indexes.retain(|v| v.as_str() != Some(entry_hex.as_str()));
    if indexes.len() == original_len {
        return Ok(());
    }

    if indexes.is_empty() {
        view.erase(&root_key)
            .map_err(|_| TransactionResult::TefInternal)?;
    } else {
        let new_bytes =
            serde_json::to_vec(&dir).map_err(|_| TransactionResult::TefInternal)?;
        view.update(root_key, new_bytes)
            .map_err(|_| TransactionResult::TefInternal)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_codec::address::classic::decode_account_id;
    use rxrpl_ledger::Ledger;

    const ACCT: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn id() -> AccountId {
        decode_account_id(ACCT).unwrap()
    }

    fn entry(byte: u8) -> Hash256 {
        let mut bytes = [0u8; 32];
        bytes[31] = byte;
        Hash256::from(bytes)
    }

    fn fresh_sandbox() -> (Ledger, FeeSettings) {
        let ledger = Ledger::genesis();
        (ledger, FeeSettings::default())
    }

    #[test]
    fn add_creates_root_page() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        let e = entry(1);
        add_to_owner_dir(&mut sandbox, &account, &e).unwrap();

        let dir_bytes = sandbox.read(&keylet::owner_dir(&account)).unwrap();
        let dir: Value = serde_json::from_slice(&dir_bytes).unwrap();
        assert_eq!(dir["Indexes"][0], e.to_string());
    }

    #[test]
    fn add_appends_to_existing_page() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        add_to_owner_dir(&mut sandbox, &account, &entry(1)).unwrap();
        add_to_owner_dir(&mut sandbox, &account, &entry(2)).unwrap();

        let dir_bytes = sandbox.read(&keylet::owner_dir(&account)).unwrap();
        let dir: Value = serde_json::from_slice(&dir_bytes).unwrap();
        assert_eq!(dir["Indexes"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn add_is_idempotent() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        let e = entry(7);
        add_to_owner_dir(&mut sandbox, &account, &e).unwrap();
        add_to_owner_dir(&mut sandbox, &account, &e).unwrap();

        let dir: Value =
            serde_json::from_slice(&sandbox.read(&keylet::owner_dir(&account)).unwrap()).unwrap();
        assert_eq!(dir["Indexes"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn remove_clears_entry_and_keeps_others() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        add_to_owner_dir(&mut sandbox, &account, &entry(1)).unwrap();
        add_to_owner_dir(&mut sandbox, &account, &entry(2)).unwrap();
        remove_from_owner_dir(&mut sandbox, &account, &entry(1)).unwrap();

        let dir: Value =
            serde_json::from_slice(&sandbox.read(&keylet::owner_dir(&account)).unwrap()).unwrap();
        assert_eq!(dir["Indexes"].as_array().unwrap().len(), 1);
        assert_eq!(dir["Indexes"][0], entry(2).to_string());
    }

    #[test]
    fn remove_last_erases_page() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        let e = entry(9);
        add_to_owner_dir(&mut sandbox, &account, &e).unwrap();
        remove_from_owner_dir(&mut sandbox, &account, &e).unwrap();

        assert!(sandbox.read(&keylet::owner_dir(&account)).is_none());
    }

    #[test]
    fn remove_non_existent_is_noop() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        remove_from_owner_dir(&mut sandbox, &account, &entry(1)).unwrap();
        // Still nothing in the directory; no panic.
        assert!(sandbox.read(&keylet::owner_dir(&account)).is_none());
    }

    #[test]
    fn full_page_returns_tec_dir_full() {
        let (ledger, fees) = fresh_sandbox();
        let view = LedgerView::with_fees(&ledger, fees);
        let mut sandbox = Sandbox::new(&view);

        let account = id();
        for i in 0..MAX_ENTRIES_PER_PAGE {
            add_to_owner_dir(&mut sandbox, &account, &entry(i as u8)).unwrap();
        }
        let err = add_to_owner_dir(&mut sandbox, &account, &entry(0xff)).unwrap_err();
        assert_eq!(err, TransactionResult::TecDirFull);
    }
}
